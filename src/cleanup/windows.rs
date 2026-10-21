//! All destructive Windows operations are confined here.
//!
//! Every ancestor is opened without write/delete sharing, checked for reparse
//! attributes and kept alive until the leaf is closed. This pins the namespace
//! while subsequent path components are opened. The final file is exclusive;
//! identity and metadata are checked on that handle before disposition is set.
use super::{Identity, Snapshot, normal_path};
use anyhow::{Context, Result, ensure};
use std::{fs::File, mem::size_of, os::windows::{ffi::OsStrExt,io::{AsRawHandle,FromRawHandle}},path::Path};
use ::windows::{core::PCWSTR,Win32::{Foundation::HANDLE,Storage::FileSystem::*}};

fn wide(value: &str) -> Vec<u16> {std::ffi::OsStr::new(value).encode_wide().chain(Some(0)).collect()}
fn handle(file: &File) -> HANDLE {HANDLE(file.as_raw_handle())}
fn open(path: &str, directory: bool, delete: bool) -> Result<File> {
    let path=wide(&format!(r"\\?\{}",path.strip_prefix(r"\\?\").unwrap_or(path)));
    // Attribute-only opens do not enforce directory sharing restrictions on NTFS.
    // LIST_DIRECTORY is necessary to prevent rename/reparse mutation while pinned.
    let access=FILE_READ_ATTRIBUTES.0 | if directory {FILE_LIST_DIRECTORY.0} else {0} | if delete {DELETE.0} else {0};
    let share=if directory {FILE_SHARE_READ} else if delete {FILE_SHARE_MODE(0)} else {FILE_SHARE_READ|FILE_SHARE_WRITE|FILE_SHARE_DELETE};
    let h=unsafe {CreateFileW(PCWSTR(path.as_ptr()),access,share,None,OPEN_EXISTING,
        FILE_FLAG_OPEN_REPARSE_POINT|FILE_FLAG_BACKUP_SEMANTICS,None)}.context("Open object without following reparse points")?;
    Ok(unsafe {File::from_raw_handle(h.0)})
}
fn info<T: Default>(file: &File, class: FILE_INFO_BY_HANDLE_CLASS)->Result<T> {
    let mut value=T::default();
    unsafe {GetFileInformationByHandleEx(handle(file),class,(&mut value as *mut T).cast(),size_of::<T>() as u32)}?;
    Ok(value)
}
fn identity(file: &File)->Result<Identity> {
    let id: FILE_ID_INFO=info(file,FileIdInfo)?;
    Ok(Identity {volume:id.VolumeSerialNumber,id:id.FileId.Identifier})
}
fn ordinary_directory(file: &File)->Result<()> {
    let basic:FILE_BASIC_INFO=info(file,FileBasicInfo)?;
    ensure!(basic.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT.0 == 0,"reparse_point");
    ensure!(basic.FileAttributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0,"not_directory");
    Ok(())
}

pub struct DirectoryGuard { files: Vec<File>, pub identities:Vec<Identity>, pub path:String }
impl DirectoryGuard {
    pub fn open(path:&str)->Result<Self> {
        normal_path(path)?;
        let path=path.strip_prefix(r"\\?\").unwrap_or(path).trim_end_matches('\\');
        let path=if path.len()==2 {format!("{path}\\")} else {path.into()};
        let mut files=Vec::new();let mut identities=Vec::new();
        let mut current=path[..3].to_owned();
        let root=open(&current,true,false)?; ordinary_directory(&root)?;
        identities.push(identity(&root)?);files.push(root);
        for part in path[3..].split('\\').filter(|p|!p.is_empty()) {
            if !current.ends_with('\\') {current.push('\\');}current.push_str(part);
            let file=open(&current,true,false)?;ordinary_directory(&file)?;
            identities.push(identity(&file)?);files.push(file);
        }
        Ok(Self {files,identities,path})
    }
    pub fn volume_id(&self)->String {self.identities[0].volume_id()}
    pub fn canonical(&self)->Result<String> {
        let file=self.files.last().unwrap();let mut buf=vec![0u16;32768];
        let len=unsafe {GetFinalPathNameByHandleW(handle(file),&mut buf,FILE_NAME_NORMALIZED)} as usize;
        ensure!(len>0 && len<buf.len(),"Read canonical directory path");
        let value=String::from_utf16(&buf[..len])?;
        Ok(value.strip_prefix(r"\\?\").unwrap_or(&value).to_owned())
    }
}
pub fn canonical_directory(path:&str)->Result<String> {DirectoryGuard::open(path)?.canonical()}
pub fn volume_id(path:&str)->Result<String> {Ok(DirectoryGuard::open(path)?.volume_id())}

fn snapshot(file:&File, parents:Vec<Identity>)->Result<Snapshot> {
    let basic:FILE_BASIC_INFO=info(file,FileBasicInfo)?;
    let standard:FILE_STANDARD_INFO=info(file,FileStandardInfo)?;
    ensure!(!standard.Directory,"directory");
    ensure!(!standard.DeletePending,"delete_pending");
    ensure!(standard.NumberOfLinks==1,"multiple_hardlinks");
    let blocked=FILE_ATTRIBUTE_REPARSE_POINT.0|FILE_ATTRIBUTE_OFFLINE.0|FILE_ATTRIBUTE_READONLY.0|
        FILE_ATTRIBUTE_SYSTEM.0|FILE_ATTRIBUTE_RECALL_ON_OPEN.0|FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS.0;
    ensure!(basic.FileAttributes&blocked==0,"special_attributes");
    Ok(Snapshot {identity:identity(file)?,parents,modified_ticks:basic.LastWriteTime,change_ticks:basic.ChangeTime,
        logical_bytes:standard.EndOfFile.max(0) as u64,allocated_bytes:standard.AllocationSize.max(0) as u64,
        attributes:basic.FileAttributes})
}
pub fn inspect(path:&str)->Result<Snapshot> {
    let parent=Path::new(path).parent().context("Missing parent")?.to_str().context("Non-Unicode path")?;
    let guard=DirectoryGuard::open(parent)?;
    let file=open(path,false,false)?;
    snapshot(&file,guard.identities.clone())
}

pub fn delete_verified(path:&str, expected:&Snapshot)->Result<()> {
    let parent=Path::new(path).parent().context("Missing parent")?.to_str().context("Non-Unicode path")?;
    let guard=DirectoryGuard::open(parent)?;
    ensure!(guard.identities==expected.parents,"parent_changed");
    let file=open(path,false,true)?;
    let actual=snapshot(&file,guard.identities.clone())?;
    ensure!(&actual==expected,"file_changed");
    let disposition=FILE_DISPOSITION_INFO {DeleteFile:true};
    unsafe {SetFileInformationByHandle(handle(&file),FileDispositionInfo,
        (&disposition as *const FILE_DISPOSITION_INFO).cast(),size_of::<FILE_DISPOSITION_INFO>() as u32)}
        .context("Delete verified file handle")?;
    drop(file); // disposition applies before the pinned ancestors are released
    drop(guard);
    Ok(())
}
