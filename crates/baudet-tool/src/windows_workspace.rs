use cap_std::{
    ambient_authority,
    fs::{Dir, OpenOptions},
};
use same_file::Handle;
use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};

const MAX_ENTRIES: usize = 40_000;
const MAX_BYTES: u64 = 256 * 1024 * 1024;
const MAX_DEPTH: usize = 128;

#[derive(Debug, PartialEq, Eq)]
pub enum Entry {
    Directory,
    File { bytes: Vec<u8>, mode: u32 },
}

pub struct Workspace {
    root: PathBuf,
    original: Dir,
    copy: tempfile::TempDir,
    baseline: BTreeMap<PathBuf, Entry>,
}

impl Workspace {
    /// Take a bounded snapshot through directory handles. The temporary directory
    /// belongs to Baudet until a sandbox grants a container access to it.
    pub fn stage(root: &Path) -> io::Result<Self> {
        Self::stage_in(root, &std::env::temp_dir())
    }

    pub(crate) fn stage_in(root: &Path, parent: &Path) -> io::Result<Self> {
        let root = root.canonicalize()?;
        let original = Dir::open_ambient_dir(&root, ambient_authority())?;
        let baseline = snapshot(&original)?;
        let copy = tempfile::Builder::new().prefix("baudet-project-").tempdir_in(parent)?;
        let destination = Dir::open_ambient_dir(copy.path(), ambient_authority())?;
        for (path, entry) in &baseline {
            match entry {
                Entry::Directory => destination.create_dir(path)?,
                Entry::File { bytes, mode } => {
                    let mut file = destination.open_with(path, OpenOptions::new().write(true).create_new(true))?;
                    file.write_all(bytes)?;
                    set_file_mode(&file, *mode)?;
                }
            }
        }
        Ok(Self { root, original, copy, baseline })
    }

    pub fn path(&self) -> &Path {
        self.copy.path()
    }

    /// Validate the complete candidate before returning any writes. An empty
    /// update requires no change to the project.
    pub fn changes(&self) -> io::Result<BTreeMap<PathBuf, Option<Entry>>> {
        let sandbox = Dir::open_ambient_dir(self.copy.path(), ambient_authority())?;
        let candidate = snapshot(&sandbox)?;
        let mut updates = BTreeMap::new();
        for path in self.baseline.keys() {
            if !candidate.contains_key(path) {
                updates.insert(path.clone(), None);
            }
        }
        for (path, entry) in candidate {
            if self.baseline.get(&path) != Some(&entry) {
                updates.insert(path, Some(entry));
            }
        }

        if !updates.is_empty() {
            let current = Dir::open_ambient_dir(&self.root, ambient_authority())?;
            let identity = |dir: &Dir| Handle::from_file(dir.try_clone()?.into_std_file());
            if identity(&current)? != identity(&self.original)? || snapshot(&current)? != self.baseline {
                return Err(io::Error::other("project changed while the sandbox ran"));
            }
        }

        Ok(updates)
    }

    /// Stage every replacement before changing the project. Keep removed files
    /// available until all project operations succeed.
    pub fn commit(&self) -> io::Result<()> {
        let changes = self.changes()?;
        if changes.is_empty() {
            return Ok(());
        }
        let scratch_path =
            Path::new(self.copy.path().file_name().ok_or_else(|| io::Error::other("missing workspace name"))?);
        self.original.create_dir(scratch_path)?;
        let scratch = self.original.open_dir(scratch_path)?;
        let mut undo = Vec::new();
        let result = (|| {
            #[cfg(not(windows))]
            for (index, (_, entry)) in changes.iter().enumerate() {
                if let Some(Entry::File { bytes, mode }) = entry {
                    let mut file =
                        scratch.open_with(format!("new-{index}"), OpenOptions::new().write(true).create_new(true))?;
                    file.write_all(bytes)?;
                    set_file_mode(&file, *mode)?;
                    file.sync_all()?;
                }
            }

            // Persist the recovery mapping before moving any original file.
            // The in-memory undo list still handles ordinary operation errors.
            let backups = changes
                .iter()
                .enumerate()
                .filter_map(|(index, (path, _))| {
                    matches!(self.baseline.get(path), Some(Entry::File { .. })).then(|| (format!("old-{index}"), path))
                })
                .collect::<BTreeMap<_, _>>();
            let mut manifest = scratch.open_with("backups.json", OpenOptions::new().write(true).create_new(true))?;
            serde_json::to_writer(&mut manifest, &backups)?;
            manifest.sync_all()?;
            drop(manifest);
            #[cfg(unix)]
            {
                scratch.try_clone()?.into_std_file().sync_all()?;
                self.original.try_clone()?.into_std_file().sync_all()?;
            }

            // Remove children before parents. Preserve the original file itself,
            // including its metadata, until the operation is committed.
            for (index, (path, replacement)) in changes.iter().enumerate().rev() {
                match self.baseline.get(path) {
                    Some(Entry::File { bytes: expected, mode }) => {
                        let metadata = self.original.symlink_metadata(path)?;
                        if !metadata.is_file()
                            || is_link(&metadata)
                            || metadata.len() != expected.len() as u64
                            || file_mode(&metadata) != *mode
                        {
                            return Err(io::Error::other("project file changed before writeback"));
                        }
                        let mut current = Vec::new();
                        self.original.open(path)?.take(expected.len() as u64 + 1).read_to_end(&mut current)?;
                        if current != *expected {
                            return Err(io::Error::other("project file changed before writeback"));
                        }
                        let backup = format!("old-{index}");
                        self.original.rename(path, &scratch, &backup)?;
                        undo.push(Undo::RestoreFile(path, backup));
                    }
                    Some(Entry::Directory)
                        if replacement.is_none() || matches!(replacement, Some(Entry::File { .. })) =>
                    {
                        let metadata = self.original.symlink_metadata(path)?;
                        if !metadata.is_dir() || is_link(&metadata) {
                            return Err(io::Error::other("project directory changed before writeback"));
                        }
                        self.original.remove_dir(path)?;
                        undo.push(Undo::RestoreDirectory(path));
                    }
                    _ => {}
                }
            }

            // Create parents before children. The destination must remain absent
            // so an independent concurrent creation is never overwritten.
            for (index, (path, entry)) in changes.iter().enumerate() {
                match entry {
                    Some(Entry::File { bytes, .. }) => {
                        #[cfg(windows)]
                        {
                            use cap_std::fs::OpenOptionsExt;
                            use windows_sys::Win32::Foundation::GENERIC_WRITE;
                            use windows_sys::Win32::Storage::FileSystem::WRITE_DAC;
                            let mut options = OpenOptions::new();
                            options.create_new(true).write(true);
                            let replacing = matches!(self.baseline.get(path), Some(Entry::File { .. }));
                            if replacing {
                                options.access_mode(GENERIC_WRITE | WRITE_DAC);
                            }
                            let mut file = self.original.open_with(path, &options)?;
                            undo.push(Undo::RemoveFile(path));
                            if replacing {
                                copy_windows_dacl(&scratch, Path::new(&format!("old-{index}")), &file)?;
                            }
                            file.write_all(bytes)?;
                            file.sync_all()?;
                        }
                        #[cfg(not(windows))]
                        {
                            let _ = bytes;
                            scratch.hard_link(format!("new-{index}"), &self.original, path)?;
                            undo.push(Undo::RemoveFile(path));
                        }
                    }
                    Some(Entry::Directory) => {
                        self.original.create_dir(path)?;
                        undo.push(Undo::RemoveDirectory(path));
                    }
                    None => {}
                }
            }
            Ok(())
        })();

        if let Err(error) = result {
            let mut recovery_failed = false;
            for change in undo.into_iter().rev() {
                let restored = match change {
                    Undo::RestoreFile(path, backup) => scratch.hard_link(backup, &self.original, path),
                    Undo::RestoreDirectory(path) => self.original.create_dir(path),
                    Undo::RemoveFile(path) => self.original.remove_file(path),
                    Undo::RemoveDirectory(path) => self.original.remove_dir(path),
                };
                recovery_failed |= restored.is_err();
            }
            if recovery_failed {
                return Err(io::Error::other(format!(
                    "project writeback failed: {error}; recovery files remain in {}",
                    scratch_path.display()
                )));
            }
            drop(scratch);
            let _ = self.original.remove_dir_all(scratch_path);
            return Err(error);
        }
        drop(scratch);
        self.original.remove_dir_all(scratch_path)
    }
}

#[cfg(windows)]
fn copy_windows_dacl(original: &Dir, path: &Path, output: &cap_std::fs::File) -> io::Result<()> {
    use cap_std::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Security::{GetKernelObjectSecurity, SetKernelObjectSecurity, DACL_SECURITY_INFORMATION};
    use windows_sys::Win32::Storage::FileSystem::READ_CONTROL;

    let source = original.open_with(path, OpenOptions::new().access_mode(READ_CONTROL))?;
    let mut size = 0;
    unsafe {
        GetKernelObjectSecurity(source.as_raw_handle(), DACL_SECURITY_INFORMATION, std::ptr::null_mut(), 0, &mut size);
    }
    if size == 0 {
        return Err(io::Error::last_os_error());
    }
    // Use aligned storage for the self-relative security descriptor.
    let mut descriptor = vec![0u32; (size as usize).div_ceil(size_of::<u32>())];
    let descriptor = descriptor.as_mut_ptr().cast();
    let mut written = size;
    if unsafe {
        GetKernelObjectSecurity(source.as_raw_handle(), DACL_SECURITY_INFORMATION, descriptor, size, &mut written)
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    if unsafe { SetKernelObjectSecurity(output.as_raw_handle(), DACL_SECURITY_INFORMATION, descriptor) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

enum Undo<'a> {
    RestoreFile(&'a Path, String),
    RestoreDirectory(&'a Path),
    RemoveFile(&'a Path),
    RemoveDirectory(&'a Path),
}

fn snapshot(dir: &Dir) -> io::Result<BTreeMap<PathBuf, Entry>> {
    fn walk(
        root: &Dir,
        dir: &Dir,
        prefix: &Path,
        depth: usize,
        bytes_left: &mut u64,
        entries: &mut BTreeMap<PathBuf, Entry>,
    ) -> io::Result<()> {
        if depth > MAX_DEPTH {
            return Err(io::Error::other("workspace exceeds the maximum directory depth"));
        }

        for item in dir.entries()? {
            if entries.len() >= MAX_ENTRIES {
                return Err(io::Error::other("workspace exceeds the maximum entry count"));
            }
            let name = item?.file_name();
            let mut components = Path::new(&name).components();
            if !matches!(components.next(), Some(Component::Normal(_)))
                || components.next().is_some()
                || name.to_string_lossy().contains(':')
            {
                return Err(io::Error::other("workspace contains an unsupported entry name"));
            }
            let path = prefix.join(name);
            let metadata = root.symlink_metadata(&path)?;
            if is_link(&metadata) {
                return Err(io::Error::other(format!("workspace contains a link at {}", path.display())));
            }
            if metadata.is_dir() {
                let subdir = root.open_dir(&path)?;
                entries.insert(path.clone(), Entry::Directory);
                walk(root, &subdir, &path, depth + 1, bytes_left, entries)?;
            } else if metadata.is_file() {
                let file = root.open(&path)?;
                let metadata = file.metadata()?;
                if !metadata.is_file() || is_link(&metadata) {
                    return Err(io::Error::other(format!("workspace entry changed at {}", path.display())));
                }
                if metadata.len() > *bytes_left {
                    return Err(io::Error::other("workspace exceeds the maximum snapshot size"));
                }
                let mut data = Vec::new();
                file.take(*bytes_left + 1).read_to_end(&mut data)?;
                *bytes_left = bytes_left
                    .checked_sub(data.len() as u64)
                    .ok_or_else(|| io::Error::other("workspace exceeds the maximum snapshot size"))?;
                entries.insert(path, Entry::File { bytes: data, mode: file_mode(&metadata) });
            } else {
                return Err(io::Error::other(format!("unsupported workspace entry {}", path.display())));
            }
        }
        Ok(())
    }

    let mut entries = BTreeMap::new();
    let mut bytes_left = MAX_BYTES;
    walk(dir, dir, Path::new(""), 0, &mut bytes_left, &mut entries)?;
    Ok(entries)
}

#[cfg(unix)]
fn file_mode(metadata: &cap_std::fs::Metadata) -> u32 {
    use cap_std::fs::MetadataExt;
    metadata.mode() & 0o777
}

#[cfg(not(unix))]
fn file_mode(_metadata: &cap_std::fs::Metadata) -> u32 {
    0
}

#[cfg(unix)]
fn set_file_mode(file: &cap_std::fs::File, mode: u32) -> io::Result<()> {
    use cap_std::fs::{Permissions, PermissionsExt};
    file.set_permissions(Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_file_mode(_file: &cap_std::fs::File, _mode: u32) -> io::Result<()> {
    Ok(())
}

fn is_link(metadata: &cap_std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use cap_std::fs::MetadataExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return true;
        }
    }
    metadata.is_symlink()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[cfg(windows)]
    #[test]
    fn given_windows_file_acls_when_the_copy_is_committed_then_replacements_keep_and_creations_inherit_them() {
        let project = tempfile::tempdir().unwrap();
        let nested = project.path().join("nested");
        fs::create_dir(&nested).unwrap();
        let original = nested.join("original.txt");
        fs::write(&original, b"before").unwrap();
        let icacls = PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32").join("icacls.exe");
        let acl = |path: &Path, args: &[&str]| {
            let result = std::process::Command::new(&icacls).arg(path).args(args).output().unwrap();
            assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
            String::from_utf8_lossy(&result.stdout).into_owned()
        };
        acl(&nested, &["/deny", "*S-1-5-32-545:(OI)(CI)(X)"]);
        acl(&original, &["/deny", "*S-1-5-32-545:(WA)"]);
        let before = acl(&original, &[]);
        let workspace = Workspace::stage(project.path()).unwrap();
        fs::write(workspace.path().join("nested/original.txt"), b"after").unwrap();
        fs::write(workspace.path().join("nested/new.txt"), b"new").unwrap();

        workspace.commit().unwrap();

        assert_eq!(acl(&original, &[]), before);
        let inherited = acl(&nested.join("new.txt"), &[]);
        assert!(inherited.lines().any(|line| line.contains("(I)(DENY)") && line.contains("(X)")), "{inherited}");
        assert_eq!(fs::read(&original).unwrap(), b"after");
    }

    #[cfg(windows)]
    #[test]
    fn given_a_staged_project_when_the_copy_contains_a_junction_then_no_updates_are_returned() {
        let project = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("untouched"), b"outside").unwrap();
        let workspace = Workspace::stage(project.path()).unwrap();
        let junction = workspace.path().join("junction");
        let cmd = PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32").join("cmd.exe");
        let result = std::process::Command::new(cmd)
            .args(["/d", "/c", "mklink", "/j"])
            .arg(&junction)
            .arg(outside.path())
            .output()
            .unwrap();
        assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));

        let error = workspace.changes().unwrap_err();
        fs::remove_dir(junction).unwrap();
        assert!(error.to_string().contains("workspace contains a link"));
        assert_eq!(fs::read(outside.path().join("untouched")).unwrap(), b"outside");
    }

    #[test]
    fn given_a_staged_project_when_the_copy_changes_then_only_its_changes_are_reported() {
        let project = tempfile::tempdir().unwrap();
        fs::write(project.path().join("a"), "before").unwrap();
        let workspace = Workspace::stage(project.path()).unwrap();
        fs::write(workspace.path().join("a"), "after").unwrap();

        assert_eq!(
            workspace.changes().unwrap(),
            BTreeMap::from([(
                PathBuf::from("a"),
                Some(Entry::File {
                    bytes: b"after".to_vec(),
                    mode: file_mode(&workspace.original.metadata("a").unwrap())
                })
            )])
        );
        assert_eq!(fs::read(project.path().join("a")).unwrap(), b"before");
    }

    #[test]
    fn given_a_staged_project_when_the_original_changes_then_no_updates_are_returned() {
        let project = tempfile::tempdir().unwrap();
        fs::write(project.path().join("a"), "before").unwrap();
        let workspace = Workspace::stage(project.path()).unwrap();
        fs::write(workspace.path().join("a"), "sandbox update").unwrap();
        fs::write(project.path().join("a"), "outside update").unwrap();

        assert!(workspace.changes().unwrap_err().to_string().contains("project changed"));
        assert_eq!(fs::read(project.path().join("a")).unwrap(), b"outside update");
    }

    #[test]
    fn given_one_changed_file_when_committed_then_the_project_has_the_new_contents() {
        let project = tempfile::tempdir().unwrap();
        fs::write(project.path().join("a"), b"before").unwrap();
        let workspace = Workspace::stage(project.path()).unwrap();
        fs::write(workspace.path().join("a"), b"after").unwrap();

        workspace.commit().unwrap();

        assert_eq!(fs::read(project.path().join("a")).unwrap(), b"after");
        assert_eq!(fs::read_dir(project.path()).unwrap().count(), 1);
    }

    #[test]
    fn given_one_changed_file_and_a_conflict_when_committed_then_the_original_is_preserved() {
        let project = tempfile::tempdir().unwrap();
        fs::write(project.path().join("a"), b"before").unwrap();
        let workspace = Workspace::stage(project.path()).unwrap();
        fs::write(workspace.path().join("a"), b"sandbox").unwrap();
        fs::write(project.path().join("a"), b"outside").unwrap();

        assert!(workspace.commit().is_err());
        assert_eq!(fs::read(project.path().join("a")).unwrap(), b"outside");
        assert_eq!(fs::read_dir(project.path()).unwrap().count(), 1);
    }
}
