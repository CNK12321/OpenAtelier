//! The projects folder as a library: the projects in it, the folders they're sorted
//! into, the archive and the trash — all of it plain folders on disk, so it's the same
//! wherever the projects folder is opened (another computer, a backup).
//!
//! ```text
//! <projects>/<project>/<project>.oaproj.json        a project, not in a folder
//! <projects>/<Folder>/<project>/<project>.oaproj.json   in a folder of your own
//! <projects>/.archive/<project>/…                    archived: kept, out of the way
//! <projects>/.trash/<project>/…                      in the trash (with a `.origin` note
//!                                                    of where it came from)
//! ```
//!
//! A folder holding a project file is a project; one that doesn't is a folder of them.
//! Moving a project moves its whole folder (its media stay where they are — projects
//! point at them). Nothing here deletes a project except emptying the trash.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub const ARCHIVE: &str = ".archive";
pub const TRASH: &str = ".trash";
/// In a trashed project's folder: where it was (relative to the projects folder).
const ORIGIN: &str = ".origin";
const EXTENSION: &str = ".oaproj.json";

/// Where a project is.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Place {
    /// In the projects folder, not in a folder.
    Top,
    /// In one of your folders.
    Folder(String),
    Archive,
    Trash,
    /// Somewhere else on disk (saved there with Save as…; known from the recent list).
    Elsewhere,
}

impl Place {
    /// Where its folder goes inside the projects folder (`None`: it can't be moved there).
    fn dir(&self, root: &Path) -> Option<PathBuf> {
        match self {
            Place::Top => Some(root.to_path_buf()),
            Place::Folder(f) => Some(root.join(f)),
            Place::Archive => Some(root.join(ARCHIVE)),
            Place::Trash => Some(root.join(TRASH)),
            Place::Elsewhere => None,
        }
    }

    /// As written in a trashed project's `.origin` note.
    fn note(&self) -> String {
        match self {
            Place::Folder(f) => f.clone(),
            Place::Archive => ARCHIVE.into(),
            _ => String::new(),
        }
    }

    fn from_note(note: &str) -> Place {
        match note.trim() {
            "" => Place::Top,
            ARCHIVE => Place::Archive,
            f => Place::Folder(f.to_string()),
        }
    }
}

/// One project in the library.
#[derive(Clone, Debug)]
pub struct Entry {
    /// Its project file.
    pub file: PathBuf,
    pub name: String,
    pub place: Place,
    /// When its file was last written.
    pub edited: SystemTime,
    /// When its folder was made (its file's time where that isn't known).
    pub created: SystemTime,
}

/// What the projects folder holds: every project, and your folders (sorted by name).
#[derive(Clone, Debug, Default)]
pub struct Library {
    pub projects: Vec<Entry>,
    pub folders: Vec<String>,
}

/// The project file in `dir`, if it holds one.
pub fn project_file_in(dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).find(|p| p.is_file() && p.file_name().is_some_and(|n| n.to_string_lossy().ends_with(EXTENSION)))
}

/// A project's name from its file: "Trip.oaproj.json" → "Trip".
pub fn name_of(file: &Path) -> String {
    let n = file.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    n.strip_suffix(EXTENSION).map(str::to_string).unwrap_or(n)
}

fn entry(file: PathBuf, place: Place) -> Entry {
    let meta = std::fs::metadata(&file).ok();
    let edited = meta.as_ref().and_then(|m| m.modified().ok()).unwrap_or(SystemTime::UNIX_EPOCH);
    let created = file.parent().and_then(|d| std::fs::metadata(d).ok()).and_then(|m| m.created().ok()).unwrap_or(edited);
    Entry { name: name_of(&file), file, place, edited, created }
}

/// Every project in `root` and the folders they're in.
pub fn scan(root: &Path) -> Library {
    let mut lib = Library::default();
    let Ok(dirs) = std::fs::read_dir(root) else { return lib };
    let mut dirs: Vec<PathBuf> = dirs.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    dirs.sort();
    let projects_in = |dir: &Path, place: Place, out: &mut Vec<Entry>| {
        let Ok(inner) = std::fs::read_dir(dir) else { return };
        for d in inner.flatten().map(|e| e.path()).filter(|p| p.is_dir()) {
            if let Some(file) = project_file_in(&d) {
                out.push(entry(file, place.clone()));
            }
        }
    };
    for dir in dirs {
        let name = dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        if name == ARCHIVE {
            projects_in(&dir, Place::Archive, &mut lib.projects);
        } else if name == TRASH {
            projects_in(&dir, Place::Trash, &mut lib.projects);
        } else if name.starts_with('.') {
            continue;
        } else if let Some(file) = project_file_in(&dir) {
            lib.projects.push(entry(file, Place::Top));
        } else {
            projects_in(&dir, Place::Folder(name.clone()), &mut lib.projects);
            lib.folders.push(name);
        }
    }
    lib.folders.sort_by_key(|f| f.to_lowercase());
    lib
}

/// `base` in `dir`, or "`base` 2", "`base` 3"… if that's taken.
fn free_name(dir: &Path, base: &str) -> String {
    (1..).map(|n| if n == 1 { base.to_string() } else { format!("{base} {n}") }).find(|n| !dir.join(n).exists()).expect("some name is free")
}

/// Whether `file` is laid out the library's way: in a folder of its own, named like it.
fn owns_folder(file: &Path) -> bool {
    file.parent().and_then(|d| d.file_name()).is_some_and(|d| d.to_string_lossy() == name_of(file))
}

/// Makes a folder of your own; returns its name (made unique).
pub fn new_folder(root: &Path, name: &str) -> std::io::Result<String> {
    let name = free_name(root, &crate::projects::folder_name(name));
    std::fs::create_dir_all(root.join(&name))?;
    Ok(name)
}

/// Renames one of your folders; returns its new name.
pub fn rename_folder(root: &Path, old: &str, new: &str) -> std::io::Result<String> {
    let wanted = crate::projects::folder_name(new);
    if wanted == old {
        return Ok(wanted);
    }
    let name = free_name(root, &wanted);
    std::fs::rename(root.join(old), root.join(&name))?;
    Ok(name)
}

/// Takes a folder away, keeping what's in it: its projects go back to the top.
pub fn remove_folder(root: &Path, folder: &str) -> std::io::Result<()> {
    let dir = root.join(folder);
    for d in std::fs::read_dir(&dir)?.flatten().map(|e| e.path()).filter(|p| p.is_dir()) {
        let name = free_name(root, &d.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default());
        std::fs::rename(&d, root.join(name))?;
    }
    std::fs::remove_dir(&dir)
}

/// The project file of `file`'s folder once it's at `dir/<folder>` (its folder's name
/// kept, or made unique there).
fn moved_into(file: &Path, dir: &Path) -> std::io::Result<PathBuf> {
    let from = file.parent().ok_or_else(|| std::io::Error::other("a project file with no folder"))?;
    let folder = from.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    std::fs::create_dir_all(dir)?;
    let name = free_name(dir, &folder);
    let to = dir.join(&name);
    std::fs::rename(from, &to)?;
    Ok(to.join(file.file_name().expect("a file")))
}

/// Moves a project (its whole folder) to `to`; returns where its file is now. A project
/// going to the trash notes where it came from (`from`), for putting it back.
pub fn move_project(root: &Path, file: &Path, from: &Place, to: &Place) -> std::io::Result<PathBuf> {
    if !owns_folder(file) || *from == Place::Elsewhere {
        return Err(std::io::Error::other("only projects in the projects folder can be moved"));
    }
    let dir = to.dir(root).ok_or_else(|| std::io::Error::other("nowhere to move it"))?;
    let moved = moved_into(file, &dir)?;
    let folder = moved.parent().expect("in a folder");
    if *to == Place::Trash {
        std::fs::write(folder.join(ORIGIN), from.note())?;
    } else {
        let _ = std::fs::remove_file(folder.join(ORIGIN));
    }
    Ok(moved)
}

/// Where a trashed project came from (the top, if that isn't known or is gone).
pub fn origin(root: &Path, file: &Path) -> Place {
    let note = file.parent().and_then(|d| std::fs::read_to_string(d.join(ORIGIN)).ok()).unwrap_or_default();
    match Place::from_note(&note) {
        Place::Folder(f) if !root.join(&f).is_dir() => Place::Top,
        place => place,
    }
}

/// Puts a project in the trash back where it came from; returns its file there.
pub fn restore(root: &Path, file: &Path) -> std::io::Result<PathBuf> {
    let back = origin(root, file);
    move_project(root, file, &Place::Trash, &back)
}

/// Deletes a project in the trash for good (its folder; never anything outside the trash).
pub fn delete_forever(root: &Path, file: &Path) -> std::io::Result<()> {
    let folder = file.parent().ok_or_else(|| std::io::Error::other("a project file with no folder"))?;
    if folder.parent() != Some(&root.join(TRASH)) {
        return Err(std::io::Error::other("only projects in the trash are deleted"));
    }
    std::fs::remove_dir_all(folder)
}

/// Deletes everything in the trash for good; how many projects went.
pub fn empty_trash(root: &Path) -> std::io::Result<usize> {
    let trash = root.join(TRASH);
    let mut n = 0;
    for d in std::fs::read_dir(&trash).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.is_dir()) {
        std::fs::remove_dir_all(&d)?;
        n += 1;
    }
    Ok(n)
}

/// Writes `name` into a project file (its own name for itself).
fn set_name_inside(file: &Path, name: &str) -> std::io::Result<()> {
    let text = std::fs::read_to_string(file)?;
    let (mut project, _) = oa_doc::ProjectFile::load(&text).map_err(|e| std::io::Error::other(e.to_string()))?;
    project.name = name.to_string();
    let json = oa_doc::ProjectFile::to_json(&project).map_err(|e| std::io::Error::other(e.to_string()))?;
    crate::autosave::write_atomic(file, json.as_bytes())
}

/// Renames a project: the name it has for itself, its file and picture and (laid out
/// the library's way) its folder. Returns its file's new path.
pub fn rename_project(file: &Path, name: &str) -> std::io::Result<PathBuf> {
    let base = crate::projects::folder_name(name);
    set_name_inside(file, name.trim())?;
    let thumb = crate::thumbnail::path_for(file);
    let dir = file.parent().ok_or_else(|| std::io::Error::other("a project file with no folder"))?.to_path_buf();
    let folder = if owns_folder(file) && base != name_of(file) {
        let parent = dir.parent().ok_or_else(|| std::io::Error::other("a folder with no parent"))?;
        let free = free_name(parent, &base);
        std::fs::rename(&dir, parent.join(&free))?;
        parent.join(free)
    } else {
        dir
    };
    let stem = if owns_folder(file) { folder.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or(base) } else { base };
    let old = folder.join(file.file_name().expect("a file"));
    let new = folder.join(format!("{stem}{EXTENSION}"));
    if new != old {
        std::fs::rename(&old, &new)?;
        let old_thumb = folder.join(thumb.file_name().expect("a file"));
        if old_thumb.exists() {
            let _ = std::fs::rename(&old_thumb, crate::thumbnail::path_for(&new));
        }
    }
    Ok(new)
}

/// Copies a project — "<name> copy", beside it — with its picture. (Its media aren't
/// copied: both point at the same files.) Returns the copy's file.
pub fn duplicate_project(file: &Path) -> std::io::Result<PathBuf> {
    let name = format!("{} copy", name_of(file));
    let dir = file.parent().ok_or_else(|| std::io::Error::other("a project file with no folder"))?;
    let (to_dir, stem) = if owns_folder(file) {
        let parent = dir.parent().ok_or_else(|| std::io::Error::other("a folder with no parent"))?;
        let free = free_name(parent, &crate::projects::folder_name(&name));
        let to = parent.join(&free);
        copy_dir(dir, &to)?;
        // The copied file and picture, renamed for the copy.
        let copied = to.join(file.file_name().expect("a file"));
        let file_to = to.join(format!("{free}{EXTENSION}"));
        std::fs::rename(&copied, &file_to)?;
        let thumb = to.join(crate::thumbnail::path_for(file).file_name().expect("a file"));
        if thumb.exists() {
            let _ = std::fs::rename(&thumb, crate::thumbnail::path_for(&file_to));
        }
        let _ = std::fs::remove_file(to.join(ORIGIN));
        (to, free)
    } else {
        // Saved somewhere of its own: the copy goes beside it, as a file.
        let stem = (1..).map(|n| if n == 1 { name.clone() } else { format!("{name} {n}") }).find(|s| !dir.join(format!("{s}{EXTENSION}")).exists()).expect("free");
        std::fs::copy(file, dir.join(format!("{stem}{EXTENSION}")))?;
        let thumb = crate::thumbnail::path_for(file);
        if thumb.exists() {
            let _ = std::fs::copy(&thumb, crate::thumbnail::path_for(&dir.join(format!("{stem}{EXTENSION}"))));
        }
        (dir.to_path_buf(), stem)
    };
    let copy = to_dir.join(format!("{stem}{EXTENSION}"));
    set_name_inside(&copy, &name)?;
    Ok(copy)
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)?.flatten() {
        let p = e.path();
        if p.is_dir() {
            copy_dir(&p, &to.join(e.file_name()))?;
        } else {
            std::fs::copy(&p, to.join(e.file_name()))?;
        }
    }
    Ok(())
}

/// Shows `path` in the system's file browser (selected, where that's possible).
pub fn show_in_folder(path: &Path) {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("explorer").arg(format!("/select,{}", path.display())).spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg("-R").arg(path).spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Some(dir) = path.parent() {
            let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(dir: &Path, name: &str) -> PathBuf {
        let file = crate::projects::fresh_project_file(dir, name).unwrap();
        let editor = crate::editor::Editor::with_setup(name, "landscape-16x9", oa_time::FrameRate::FPS_30);
        std::fs::write(&file, oa_doc::ProjectFile::to_json(editor.doc.project()).unwrap()).unwrap();
        std::fs::write(crate::thumbnail::path_for(&file), b"png").unwrap();
        file
    }

    fn place_of(lib: &Library, name: &str) -> Option<Place> {
        lib.projects.iter().find(|e| e.name == name).map(|e| e.place.clone())
    }

    /// Folders, moving, archiving, the trash and back, renaming and copying — all of it
    /// plain folders, found again by a scan.
    #[test]
    fn the_library_is_folders_on_disk() {
        let root = std::env::temp_dir().join(format!("oa-library-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let trip = project(&root, "Trip");
        project(&root, "Ad");
        let clients = new_folder(&root, "Clients").unwrap();
        assert_eq!(new_folder(&root, "Clients").unwrap(), "Clients 2", "names made unique");

        let lib = scan(&root);
        assert_eq!(lib.folders, ["Clients", "Clients 2"]);
        assert_eq!(place_of(&lib, "Trip"), Some(Place::Top));

        // Into a folder, to the trash (remembering where from), and back.
        let trip = move_project(&root, &trip, &Place::Top, &Place::Folder(clients.clone())).unwrap();
        assert_eq!(place_of(&scan(&root), "Trip"), Some(Place::Folder("Clients".into())));
        let trashed = move_project(&root, &trip, &Place::Folder(clients.clone()), &Place::Trash).unwrap();
        assert_eq!(place_of(&scan(&root), "Trip"), Some(Place::Trash));
        assert_eq!(origin(&root, &trashed), Place::Folder("Clients".into()));
        let back = restore(&root, &trashed).unwrap();
        assert_eq!(place_of(&scan(&root), "Trip"), Some(Place::Folder("Clients".into())));
        assert!(!back.parent().unwrap().join(ORIGIN).exists(), "the note goes once it's back");

        // Renamed: folder, file, picture and the name inside.
        let renamed = rename_project(&back, "Summer trip").unwrap();
        assert_eq!(name_of(&renamed), "Summer trip");
        assert_eq!(renamed.parent().unwrap().file_name().unwrap(), "Summer trip");
        assert!(crate::thumbnail::path_for(&renamed).exists(), "the picture follows");
        let (inside, _) = oa_doc::ProjectFile::load(&std::fs::read_to_string(&renamed).unwrap()).unwrap();
        assert_eq!(inside.name, "Summer trip");

        // Copied beside it.
        let copy = duplicate_project(&renamed).unwrap();
        assert_eq!(name_of(&copy), "Summer trip copy");
        assert_eq!(place_of(&scan(&root), "Summer trip copy"), Some(Place::Folder("Clients".into())));

        // Archived; a folder taken away puts its projects back at the top.
        move_project(&root, &copy, &Place::Folder(clients.clone()), &Place::Archive).unwrap();
        assert_eq!(place_of(&scan(&root), "Summer trip copy"), Some(Place::Archive));
        remove_folder(&root, &clients).unwrap();
        let lib = scan(&root);
        assert_eq!(place_of(&lib, "Summer trip"), Some(Place::Top));
        assert_eq!(lib.folders, ["Clients 2"]);

        // Only what's in the trash is ever deleted.
        let ad = lib.projects.iter().find(|e| e.name == "Ad").unwrap().file.clone();
        assert!(delete_forever(&root, &ad).is_err(), "not from outside the trash");
        let ad = move_project(&root, &ad, &Place::Top, &Place::Trash).unwrap();
        assert_eq!(empty_trash(&root).unwrap(), 1);
        assert!(!ad.exists());
        assert_eq!(place_of(&scan(&root), "Ad"), None);
        let _ = std::fs::remove_dir_all(&root);
    }
}
