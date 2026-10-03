use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{env, os::windows::ffi::OsStrExt};
use windows::{
    core::{BSTR, Interface, PCWSTR},
    Win32::{
        Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, VARIANT_BOOL},
        System::{
            Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize},
            TaskScheduler::{
                IActionCollection, IExecAction, ILogonTrigger, ITaskFolder, ITaskService,
                TASK_ACTION_EXEC, TASK_CREATE_OR_UPDATE, TASK_INSTANCES_IGNORE_NEW,
                TASK_LOGON_INTERACTIVE_TOKEN, TASK_RUNLEVEL_HIGHEST, TASK_TRIGGER_LOGON,
                TaskScheduler,
            },
            Variant::VARIANT,
        },
        UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_HIDE},
    },
};

// Keep the task identity stable for installations with automatic scanning enabled.
const TASK_NAME: &str = "DiskHistory Logon Scan";

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct StartupStatus {
    pub installed: bool,
    pub enabled: bool,
    pub executable: Option<String>,
}

pub fn status() -> Result<StartupStatus> {
    with_folder(|_, folder| unsafe {
        let task = match folder.GetTask(&BSTR::from(TASK_NAME)) {
            Ok(task) => task,
            Err(error) if task_missing(error.code()) => {
                return Ok(StartupStatus::default());
            }
            Err(error) => return Err(error).context("Read login scan task"),
        };
        let definition = task.Definition()?;
        let actions: IActionCollection = definition.Actions()?;
        let action: IExecAction = actions.get_Item(1)?.cast()?;
        let mut executable = BSTR::new();
        action.Path(&mut executable)?;
        Ok(StartupStatus {
            installed: true,
            enabled: task.Enabled()?.0 != 0,
            executable: Some(executable.to_string()),
        })
    })
}

pub fn install() -> Result<()> {
    let current = env::current_exe()?;
    let gui = current.with_file_name("VolumeTrail.exe");
    let exe = if current.file_name().and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case("VolumeTrail-cli.exe")
            || name.eq_ignore_ascii_case("DiskHistory-cli.exe"))
        && gui.is_file() { gui } else { current };
    let executable = exe.to_string_lossy();
    let working_directory = exe.parent().context("Executable has no parent directory")?.to_string_lossy();
    with_folder(|service, folder| unsafe {
        let user = service.ConnectedUser()?.to_string();
        let domain = service.ConnectedDomain()?.to_string();
        let account = if domain.is_empty() { user } else { format!("{domain}\\{user}") };
        let definition = service.NewTask(0)?;
        let principal = definition.Principal()?;
        principal.SetUserId(&BSTR::from(account.as_str()))?;
        principal.SetLogonType(TASK_LOGON_INTERACTIVE_TOKEN)?;
        principal.SetRunLevel(TASK_RUNLEVEL_HIGHEST)?;

        let triggers = definition.Triggers()?;
        let trigger: ILogonTrigger = triggers.Create(TASK_TRIGGER_LOGON)?.cast()?;
        trigger.SetUserId(&BSTR::from(account.as_str()))?;

        let actions = definition.Actions()?;
        let action: IExecAction = actions.Create(TASK_ACTION_EXEC)?.cast()?;
        action.SetPath(&BSTR::from(executable.as_ref()))?;
        action.SetArguments(&BSTR::from("scan-auto"))?;
        action.SetWorkingDirectory(&BSTR::from(working_directory.as_ref()))?;

        let settings = definition.Settings()?;
        settings.SetMultipleInstances(TASK_INSTANCES_IGNORE_NEW)?;
        settings.SetExecutionTimeLimit(&BSTR::from("PT0S"))?;
        settings.SetDisallowStartIfOnBatteries(VARIANT_BOOL(0))?;
        settings.SetStopIfGoingOnBatteries(VARIANT_BOOL(0))?;

        let empty = VARIANT::default();
        folder.RegisterTaskDefinition(
            &BSTR::from(TASK_NAME), &definition, TASK_CREATE_OR_UPDATE.0,
            &empty, &empty, TASK_LOGON_INTERACTIVE_TOKEN, &empty,
        ).context("Register login scan task")?;
        Ok(())
    })
}

pub fn remove() -> Result<()> {
    with_folder(|_, folder| unsafe {
        match folder.DeleteTask(&BSTR::from(TASK_NAME), 0) {
            Ok(()) => Ok(()),
            Err(error) if task_missing(error.code()) => Ok(()),
            Err(error) => Err(error).context("Remove login scan task"),
        }
    })
}

pub fn start_elevated(enable: bool) -> Result<()> {
    let exe = env::current_exe()?;
    let path: Vec<u16> = exe.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let action = if enable { "startup-enable\0" } else { "startup-disable\0" };
    let parameters: Vec<u16> = action.encode_utf16().collect();
    let verb: Vec<u16> = "runas\0".encode_utf16().collect();
    let result = unsafe {
        ShellExecuteW(None, PCWSTR(verb.as_ptr()), PCWSTR(path.as_ptr()),
            PCWSTR(parameters.as_ptr()), PCWSTR::null(), SW_HIDE)
    };
    anyhow::ensure!(result.0 as isize > 32, "Windows did not start the elevated task setup");
    Ok(())
}

fn with_folder<T>(f: impl FnOnce(&ITaskService, &ITaskFolder) -> Result<T>) -> Result<T> {
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok().context("Initialize Task Scheduler COM")?;
        let result = (|| {
            let service: ITaskService = CoCreateInstance(&TaskScheduler, None, CLSCTX_INPROC_SERVER)
                .context("Create Task Scheduler service")?;
            let empty = VARIANT::default();
            service.Connect(&empty, &empty, &empty, &empty)
                .context("Connect to Task Scheduler")?;
            let folder = service.GetFolder(&BSTR::from("\\"))
                .context("Open Task Scheduler root folder")?;
            f(&service, &folder)
        })();
        CoUninitialize();
        result
    }
}

fn task_missing(code: windows::core::HRESULT) -> bool {
    code == windows::core::HRESULT::from_win32(ERROR_FILE_NOT_FOUND.0)
        || code == windows::core::HRESULT::from_win32(ERROR_PATH_NOT_FOUND.0)
}
