use anyhow::{Context,Result};
use std::{fs::{self,OpenOptions},io::Write,os::windows::ffi::OsStrExt,path::Path};
use windows::{core::PCWSTR,Win32::Storage::FileSystem::{MoveFileExW,MOVEFILE_REPLACE_EXISTING,MOVEFILE_WRITE_THROUGH}};

pub fn write(path:&Path,bytes:&[u8])->Result<()> {
    let temporary=path.with_extension(format!("{}.tmp",crate::cleanup::new_id()));
    let result=(|| {
        let mut file=OpenOptions::new().write(true).create_new(true).open(&temporary)?;
        file.write_all(bytes)?;file.sync_all()?;drop(file);
        let src:Vec<u16>=temporary.as_os_str().encode_wide().chain(Some(0)).collect();
        let dst:Vec<u16>=path.as_os_str().encode_wide().chain(Some(0)).collect();
        unsafe {MoveFileExW(PCWSTR(src.as_ptr()),PCWSTR(dst.as_ptr()),MOVEFILE_REPLACE_EXISTING|MOVEFILE_WRITE_THROUGH)}
            .context("Atomically replace configuration")?;
        Ok(())
    })();
    if result.is_err() {let _=fs::remove_file(&temporary);}result
}
