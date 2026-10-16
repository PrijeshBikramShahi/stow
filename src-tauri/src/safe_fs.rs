//! macOS/Linux directory capabilities for creation and no-replace promotion.
use crate::core::{AppError, ErrorCode, Result};
use serde::{Deserialize, Serialize};
use std::{
    ffi::CString,
    fs::File,
    io::Read,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::{Component, Path, PathBuf},
};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct Identity {
    pub device: u64,
    pub inode: u64,
}
pub struct Directory {
    pub file: File,
    pub path: PathBuf,
}
fn error() -> AppError {
    AppError::new(ErrorCode::InvalidPath, "Directory access failed. Use an existing local directory without symlink components and check permissions.")
}
pub fn changed() -> AppError {
    AppError::new(ErrorCode::RecoveryRequired, "A directory was replaced, moved, or redirected. Data was preserved; review the recorded paths before retrying.")
}
fn cstr(value: &std::ffi::OsStr) -> Result<CString> {
    CString::new(value.as_bytes()).map_err(|_| error())
}
pub fn component(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 180
        || name.starts_with('.')
        || name.starts_with('-')
        || !name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
    {
        return Err(AppError::new(ErrorCode::InvalidPath, "Workspace name must be one component using letters, numbers, dots, underscores, or hyphens; it cannot start with a dot or hyphen."));
    }
    Ok(())
}
impl Directory {
    pub fn open(path: &Path) -> Result<Self> {
        crate::core::path_string(path)?;
        if !path.is_absolute()
            || path.as_os_str().len() > 4096
            || path
                .components()
                .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
        {
            return Err(error());
        }
        // Walk every component with O_NOFOLLOW. Canonicalizing first would hide symlinks.
        let mut directory = Self {
            file: File::open("/").map_err(|_| error())?,
            path: PathBuf::from("/"),
        };
        for part in path.components() {
            if let Component::Normal(name) = part {
                directory = directory.child_os(name)?;
            }
        }
        Ok(directory)
    }
    fn child_os(&self, name: &std::ffi::OsStr) -> Result<Self> {
        let c = cstr(name)?;
        // SAFETY: valid directory descriptor and NUL-terminated single component;
        // ownership of a successful new descriptor transfers to File exactly once.
        let fd = unsafe {
            libc::openat(
                self.file.as_raw_fd(),
                c.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(error());
        }
        Ok(Self {
            file: unsafe { File::from_raw_fd(fd) },
            path: self.path.join(name),
        })
    }
    pub fn child(&self, name: &str) -> Result<Self> {
        if name.contains('/') || name == "." || name == ".." {
            return Err(error());
        }
        self.child_os(name.as_ref())
    }
    pub fn identity(&self) -> Result<Identity> {
        let meta = self.file.metadata().map_err(|_| error())?;
        Ok(Identity {
            device: meta.dev(),
            inode: meta.ino(),
        })
    }
    pub fn validate_path(&self) -> Result<()> {
        if Self::open(&self.path)?.identity()? != self.identity()? {
            return Err(changed());
        }
        Ok(())
    }
    pub fn owned(&self) -> Result<()> {
        let meta = self.file.metadata().map_err(|_| error())?;
        // SAFETY: geteuid has no preconditions.
        if meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o022 != 0 {
            return Err(AppError::new(
                ErrorCode::PermissionDenied,
                "Choose a workspace root owned by you and not writable by other users/groups.",
            ));
        }
        #[cfg(target_os = "macos")]
        reject_extended_acl(self.file.as_raw_fd())?;
        Ok(())
    }
    pub fn lock(&self) -> Result<()> {
        // Advisory cross-process Stow admission; filesystem no-replace is still mandatory.
        if unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(AppError::new(
                ErrorCode::Busy,
                "Another Stow clone or recovery is using this directory.",
            ));
        }
        Ok(())
    }
    pub fn absent(&self, name: &str) -> Result<()> {
        let c = cstr(name.as_ref())?;
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: stat points to sufficient writable memory; name and fd are valid.
        let result = unsafe {
            libc::fstatat(
                self.file.as_raw_fd(),
                c.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if result == 0 {
            return Err(AppError::new(ErrorCode::Collision, "Destination already exists (including a symlink). Choose another name; nothing was overwritten."));
        }
        if std::io::Error::last_os_error().raw_os_error() != Some(libc::ENOENT) {
            return Err(error());
        }
        Ok(())
    }
    pub fn create(&self, name: &str) -> Result<Self> {
        self.validate_path()?;
        if name.contains('/') || name == "." || name == ".." {
            return Err(error());
        }
        let c = cstr(name.as_ref())?;
        // mkdirat is exclusive: unlike create_dir_all it never adopts existing data.
        if unsafe { libc::mkdirat(self.file.as_raw_fd(), c.as_ptr(), 0o700) } != 0 {
            return Err(AppError::new(
                ErrorCode::Collision,
                "Staging directory could not be created exclusively. Existing data was preserved.",
            ));
        }
        self.file.sync_all().map_err(AppError::io)?;
        let child = self.child(name)?;
        child.owned()?;
        Ok(child)
    }
    pub fn promote(
        &self,
        source: &str,
        expected: Identity,
        root: &Directory,
        destination: &str,
    ) -> Result<()> {
        component(destination)?;
        self.validate_path()?;
        root.validate_path()?;
        if self.child(source)?.identity()? != expected {
            return Err(changed());
        }
        let from = cstr(source.as_ref())?;
        let to = cstr(destination.as_ref())?;
        #[cfg(target_os = "macos")]
        let result = unsafe {
            libc::renameatx_np(
                self.file.as_raw_fd(),
                from.as_ptr(),
                root.file.as_raw_fd(),
                to.as_ptr(),
                libc::RENAME_EXCL,
            )
        };
        #[cfg(target_os = "linux")]
        let result = unsafe {
            libc::renameat2(
                self.file.as_raw_fd(),
                from.as_ptr(),
                root.file.as_raw_fd(),
                to.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        let result = -1;
        if result != 0 {
            return Err(AppError::new(ErrorCode::Collision, "Exclusive promotion failed. The destination may now exist or the filesystem may not support safe promotion. Staging data was preserved."));
        }
        root.file.sync_all().map_err(AppError::io)?;
        self.file.sync_all().map_err(AppError::io)?;
        if root.child(destination)?.identity()? != expected {
            return Err(changed());
        }
        Ok(())
    }
}
pub fn random_id() -> Result<String> {
    let mut bytes = [0u8; 16];
    File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .map_err(AppError::io)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

// Signatures/constants from the macOS SDK sys/acl.h. libc does not expose them.
#[cfg(target_os = "macos")]
fn reject_extended_acl(fd: libc::c_int) -> Result<()> {
    unsafe extern "C" {
        fn acl_get_fd_np(fd: libc::c_int, kind: libc::c_int) -> *mut libc::c_void;
        fn acl_get_entry(
            acl: *mut libc::c_void,
            index: libc::c_int,
            entry: *mut *mut libc::c_void,
        ) -> libc::c_int;
        fn acl_free(acl: *mut libc::c_void) -> libc::c_int;
    }
    // SAFETY: valid live fd; successful opaque ACL is freed once. FIRST_ENTRY=0,
    // TYPE_EXTENDED=0x100. Empty ACL yields -1/EINVAL on macOS (not POSIX's 0).
    let has_entries = unsafe {
        let acl = acl_get_fd_np(fd, 0x100);
        if acl.is_null() {
            // macOS reports ENOENT when this live descriptor has no extended ACL.
            return if std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOENT) {
                Ok(())
            } else {
                Err(error())
            };
        }
        let mut entry = std::ptr::null_mut();
        let result = acl_get_entry(acl, 0, &mut entry);
        let errno = std::io::Error::last_os_error().raw_os_error();
        acl_free(acl);
        if result == -1 && errno != Some(libc::EINVAL) {
            return Err(error());
        }
        result == 0
    };
    if has_entries {
        return Err(AppError::new(ErrorCode::PermissionDenied,"Choose a private workspace directory without extended ACL entries. Stow does not change existing permissions."));
    }
    Ok(())
}
