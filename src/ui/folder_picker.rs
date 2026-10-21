use anyhow::Result;
use windows::{core::{HRESULT,w},Win32::{Foundation::ERROR_CANCELLED,System::Com::{CoCreateInstance,CoInitializeEx,CoTaskMemFree,CoUninitialize,CLSCTX_INPROC_SERVER,COINIT_APARTMENTTHREADED},UI::Shell::{FileOpenDialog,IFileOpenDialog,FOS_FORCEFILESYSTEM,FOS_NOCHANGEDIR,FOS_PATHMUSTEXIST,FOS_PICKFOLDERS,SIGDN_FILESYSPATH}}};

pub(super) fn choose()->Result<Option<String>> {
    unsafe {CoInitializeEx(None,COINIT_APARTMENTTHREADED)}.ok()?;
    struct Apartment;
    impl Drop for Apartment {fn drop(&mut self) {unsafe {CoUninitialize()};}}
    let _apartment=Apartment;
    let dialog:IFileOpenDialog=unsafe {CoCreateInstance(&FileOpenDialog,None,CLSCTX_INPROC_SERVER)}?;
    unsafe {dialog.SetTitle(w!("选择清理目标目录"))?;
        dialog.SetOptions(FOS_PICKFOLDERS|FOS_FORCEFILESYSTEM|FOS_PATHMUSTEXIST|FOS_NOCHANGEDIR)?;}
    if let Err(error)=unsafe {dialog.Show(None)} {
        if error.code()==HRESULT::from_win32(ERROR_CANCELLED.0) {return Ok(None);}
        return Err(error.into());
    }
    let item=unsafe {dialog.GetResult()}?;
    let raw=unsafe {item.GetDisplayName(SIGDN_FILESYSPATH)}?;
    let path=unsafe {raw.to_string()};unsafe {CoTaskMemFree(Some(raw.0.cast()))};
    Ok(Some(path?))
}
