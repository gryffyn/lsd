mod access_control;
mod date;
mod filetype;
mod indicator;
mod inode;
mod links;
mod locale;
pub mod name;
mod owner;
mod permissions;
mod size;
mod symlink;

#[cfg(windows)]
mod windows_utils;

pub use self::access_control::AccessControl;
pub use self::date::Date;
pub use self::filetype::FileType;
pub use self::indicator::Indicator;
pub use self::inode::INode;
pub use self::links::Links;
pub use self::name::Name;
pub use self::owner::Owner;
pub use self::permissions::Permissions;
pub use self::size::Size;
pub use self::symlink::SymLink;
pub use crate::icon::Icons;

use crate::flags::{Display, Flags, Layout};
use crate::{print_error, ExitCode};

use std::io::{self, Error, ErrorKind};
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Meta {
    pub name: Name,
    pub path: PathBuf,
    pub permissions: Option<Permissions>,
    pub date: Option<Date>,
    pub owner: Option<Owner>,
    pub file_type: FileType,
    pub size: Option<Size>,
    pub symlink: SymLink,
    pub indicator: Indicator,
    pub inode: Option<INode>,
    pub links: Option<Links>,
    pub content: Option<Vec<Meta>>,
    pub access_control: Option<AccessControl>,
}

impl Meta {
    pub fn recurse_into(
        &self,
        depth: usize,
        flags: &Flags,
    ) -> io::Result<(Option<Vec<Meta>>, ExitCode)> {
        if depth == 0 {
            return Ok((None, ExitCode::OK));
        }

        if flags.display == Display::DirectoryOnly && flags.layout != Layout::Tree {
            return Ok((None, ExitCode::OK));
        }

        match self.file_type {
            FileType::Directory { .. } => (),
            FileType::SymLink { is_dir: true } => {
                if flags.layout == Layout::OneLine {
                    return Ok((None, ExitCode::OK));
                }
            }
            _ => return Ok((None, ExitCode::OK)),
        }

        let entries = match self.path.read_dir() {
            Ok(entries) => entries,
            Err(err) => {
                print_error!("{}: {}.", self.path.display(), err);
                return Ok((None, ExitCode::MinorIssue));
            }
        };

        let mut content: Vec<Meta> = Vec::new();

        if matches!(flags.display, Display::All | Display::SystemProtected)
            && flags.layout != Layout::Tree
        {
            let mut current_meta = self.clone();
            current_meta.name.name = ".".to_owned();

            let mut parent_meta =
                Self::from_path(&self.path.join(Component::ParentDir), flags.dereference.0)?;
            parent_meta.name.name = "..".to_owned();

            content.push(current_meta);
            content.push(parent_meta);
        }

        let mut exit_code = ExitCode::OK;

        for entry in entries {
            let entry = entry?;
            let path = entry.path();

            let name = path
                .file_name()
                .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "invalid file name"))?;

            if flags.ignore_globs.0.is_match(name) {
                continue;
            }

            #[cfg(windows)]
            let is_hidden =
                name.to_string_lossy().starts_with('.') || windows_utils::is_path_hidden(&path);
            #[cfg(not(windows))]
            let is_hidden = name.to_string_lossy().starts_with('.');

            #[cfg(windows)]
            let is_system = windows_utils::is_path_system(&path);
            #[cfg(not(windows))]
            let is_system = false;

            match flags.display {
                // show hidden files, but ignore system protected files
                Display::All | Display::AlmostAll if is_system => continue,
                // ignore hidden and system protected files
                Display::VisibleOnly if is_hidden || is_system => continue,
                _ => {}
            }

            let mut entry_meta = match Self::from_path(&path, flags.dereference.0) {
                Ok(res) => res,
                Err(err) => {
                    print_error!("{}: {}.", path.display(), err);
                    exit_code.set_if_greater(ExitCode::MinorIssue);
                    continue;
                }
            };

            // skip files for --tree -d
            if flags.layout == Layout::Tree
                && flags.display == Display::DirectoryOnly
                && !entry.file_type()?.is_dir()
            {
                continue;
            }

            // check dereferencing
            if flags.dereference.0 || !matches!(entry_meta.file_type, FileType::SymLink { .. }) {
                match entry_meta.recurse_into(depth - 1, flags) {
                    Ok((content, rec_exit_code)) => {
                        entry_meta.content = content;
                        exit_code.set_if_greater(rec_exit_code);
                    }
                    Err(err) => {
                        print_error!("{}: {}.", path.display(), err);
                        exit_code.set_if_greater(ExitCode::MinorIssue);
                        continue;
                    }
                };
            }

            content.push(entry_meta);
        }

        Ok((Some(content), exit_code))
    }

    pub fn calculate_total_size(&mut self) {
        if self.size.is_none() {
            return;
        }

        if let FileType::Directory { .. } = self.file_type {
            if let Some(metas) = &mut self.content {
                let mut size_accumulated = match &self.size {
                    Some(size) => size.get_bytes(),
                    None => 0,
                };
                for x in &mut metas.iter_mut() {
                    x.calculate_total_size();
                    size_accumulated += match &x.size {
                        Some(size) => size.get_bytes(),
                        None => 0,
                    };
                }
                self.size = Some(Size::new(size_accumulated));
            } else {
                // possibility that 'depth' limited the recursion in 'recurse_into'
                self.size = Some(Size::new(Meta::calculate_total_file_size(&self.path)));
            }
        }
    }

    fn calculate_total_file_size(path: &Path) -> u64 {
        let metadata = path.symlink_metadata();
        let metadata = match metadata {
            Ok(meta) => meta,
            Err(err) => {
                print_error!("{}: {}.", path.display(), err);
                return 0;
            }
        };
        let file_type = metadata.file_type();
        if file_type.is_file() {
            metadata.len()
        } else if file_type.is_dir() {
            let mut size = metadata.len();

            let entries = match path.read_dir() {
                Ok(entries) => entries,
                Err(err) => {
                    print_error!("{}: {}.", path.display(), err);
                    return size;
                }
            };
            for entry in entries {
                let path = match entry {
                    Ok(entry) => entry.path(),
                    Err(err) => {
                        print_error!("{}: {}.", path.display(), err);
                        continue;
                    }
                };
                size += Meta::calculate_total_file_size(&path);
            }
            size
        } else {
            0
        }
    }

    pub fn from_path(path: &Path, dereference: bool) -> io::Result<Self> {
        let mut metadata = path.symlink_metadata()?;
        let mut symlink_meta = None;
        let mut broken_link = false;
        if metadata.file_type().is_symlink() {
            match path.metadata() {
                Ok(m) => {
                    if dereference {
                        metadata = m;
                    } else {
                        symlink_meta = Some(m);
                    }
                }
                Err(e) => {
                    // This case, it is definitely a symlink or
                    // path.symlink_metadata would have errored out
                    if dereference {
                        broken_link = true;
                        eprintln!("lsd: {}: {}\n", path.to_str().unwrap_or(""), e);
                    }
                }
            }
        }

        #[cfg(unix)]
        let owner = Owner::from(&metadata);
        #[cfg(unix)]
        let permissions = Permissions::from(&metadata);

        #[cfg(windows)]
        let (owner, permissions) = windows_utils::get_file_data(path)?;

        #[cfg(not(windows))]
        let file_type = FileType::new(&metadata, symlink_meta.as_ref(), &permissions);

        #[cfg(windows)]
        let file_type = FileType::new(&metadata, symlink_meta.as_ref(), path);

        let name = Name::new(path, file_type);

        let (inode, links, size, date, owner, permissions, access_control) = match broken_link {
            true => (None, None, None, None, None, None, None),
            false => (
                Some(INode::from(&metadata)),
                Some(Links::from(&metadata)),
                Some(Size::from(&metadata)),
                Some(Date::from(&metadata)),
                Some(owner),
                Some(permissions),
                Some(AccessControl::for_path(path)),
            ),
        };

        Ok(Self {
            inode,
            links,
            path: path.to_path_buf(),
            symlink: SymLink::from(path),
            size,
            date,
            indicator: Indicator::from(file_type),
            owner,
            permissions,
            name,
            file_type,
            content: None,
            access_control,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::Meta;
    use std::fs::File;
    use tempfile::tempdir;

    #[cfg(unix)]
    #[test]
    fn test_from_path_path() {
        let dir = assert_fs::TempDir::new().unwrap();
        let meta = Meta::from_path(dir.path(), false).unwrap();
        assert_eq!(meta.path, dir.path())
    }

    #[test]
    fn test_from_path() {
        let tmp_dir = tempdir().expect("failed to create temp dir");

        let path_a = tmp_dir.path().join("aaa.aa");
        File::create(&path_a).expect("failed to create file");
        let meta_a = Meta::from_path(&path_a, false).expect("failed to get meta");

        let path_b = tmp_dir.path().join("bbb.bb");
        let path_c = tmp_dir.path().join("ccc.cc");

        #[cfg(unix)]
        std::os::unix::fs::symlink(path_c, &path_b).expect("failed to create broken symlink");

        // this needs to be tested on Windows
        // likely to fail because of permission issue
        // see https://doc.rust-lang.org/std/os/windows/fs/fn.symlink_file.html
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&path_c, &path_b)
            .expect("failed to create broken symlink");

        let meta_b = Meta::from_path(&path_b, true).expect("failed to get meta");

        assert!(
            meta_a.inode.is_some()
                && meta_a.links.is_some()
                && meta_a.size.is_some()
                && meta_a.date.is_some()
                && meta_a.owner.is_some()
                && meta_a.permissions.is_some()
                && meta_a.access_control.is_some()
        );

        assert!(
            meta_b.inode.is_none()
                && meta_b.links.is_none()
                && meta_b.size.is_none()
                && meta_b.date.is_none()
                && meta_b.owner.is_none()
                && meta_b.permissions.is_none()
                && meta_b.access_control.is_none()
        );
    }
}
