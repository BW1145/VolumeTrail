use anyhow::{Context, Result, ensure};
use crate::{model::{Checkpoint, Entry, Link, Mutation, ScanCounters, ScanMode, ScanOutcome, ScanProgress}, store::Store};
use ntfs::{Ntfs, NtfsAttributeType, NtfsError, NtfsFile};
use ntfs::attribute_value::NtfsAttributeValue;
use ntfs::structured_values::{NtfsAttributeList, NtfsFileName, NtfsFileNamespace};
use std::{
    ffi::c_void,
    collections::HashSet,
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    mem::size_of,
    os::windows::io::AsRawHandle,
    time::{Duration, Instant},
};
use windows::Win32::{
    Foundation::{ERROR_HANDLE_EOF, ERROR_NO_MORE_FILES, FILETIME, HANDLE},
    Storage::FileSystem::GetDiskFreeSpaceExW,
    System::{
        IO::DeviceIoControl,
        Ioctl::{
            FSCTL_ENUM_USN_DATA, FSCTL_QUERY_USN_JOURNAL, FSCTL_READ_USN_JOURNAL,
            MFT_ENUM_DATA_V0, READ_USN_JOURNAL_DATA_V0, USN_JOURNAL_DATA_V0,
        },
        Threading::GetSystemTimes,
        Performance::{PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData,
            PdhGetFormattedCounterValue, PdhOpenQueryW, PDH_FMT_COUNTERVALUE,
            PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY},
    },
};

const BUFFER_SIZE: usize = 256 * 1024;
const RECORD_HEADER_SIZE: usize = 60;

#[derive(Clone, Copy, Debug)]
pub struct Journal {
    pub id: u64,
    pub first_usn: i64,
    pub next_usn: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsnRecord {
    pub id: u64,
    pub parent: u64,
    pub usn: i64,
    pub reason: u32,
    pub attributes: u32,
    pub name: String,
}

pub struct Volume {
    file: File,
}

impl Volume {
    pub fn open(letter: char) -> Result<Self> {
        ensure!(letter.is_ascii_alphabetic(), "Expected a drive letter");
        let path = format!(r"\\.\{}:", letter.to_ascii_uppercase());
        let file = File::open(&path).with_context(|| format!("Open NTFS volume {path}; administrator rights may be required"))?;
        Ok(Self { file })
    }

    pub fn file(&self) -> &File {
        &self.file
    }

    pub fn ntfs_reader(&self) -> Result<AlignedReader> {
        Ok(AlignedReader::new(self.file.try_clone()?))
    }

    pub fn journal(&self) -> Result<Journal> {
        let mut data = USN_JOURNAL_DATA_V0::default();
        let mut written = 0;
        self.control(
            FSCTL_QUERY_USN_JOURNAL,
            None,
            Some((&mut data as *mut USN_JOURNAL_DATA_V0).cast()),
            size_of::<USN_JOURNAL_DATA_V0>() as u32,
            &mut written,
        )?;
        ensure!(written as usize >= size_of::<USN_JOURNAL_DATA_V0>(), "Short USN journal response");
        Ok(Journal { id: data.UsnJournalID, first_usn: data.FirstUsn, next_usn: data.NextUsn })
    }

    pub fn enumerate<F>(&self, high_usn: i64, max_records: Option<u64>, mut consume: F) -> Result<()>
    where
        F: FnMut(UsnRecord) -> Result<()>,
    {
        let mut input = MFT_ENUM_DATA_V0 { StartFileReferenceNumber: 0, LowUsn: 0, HighUsn: high_usn };
        let mut buffer = vec![0u8; BUFFER_SIZE];
        let mut seen = 0u64;
        loop {
            let mut written = 0;
            let result = self.control(
                FSCTL_ENUM_USN_DATA,
                Some((&input as *const MFT_ENUM_DATA_V0).cast()),
                Some(buffer.as_mut_ptr().cast()),
                buffer.len() as u32,
                &mut written,
            );
            if let Err(error) = result {
                if error.code() == windows::core::HRESULT::from_win32(ERROR_HANDLE_EOF.0)
                    || error.code() == windows::core::HRESULT::from_win32(ERROR_NO_MORE_FILES.0)
                { break; }
                return Err(error).context("Enumerate NTFS file records");
            }
            let bytes = &buffer[..written as usize];
            ensure!(bytes.len() >= 8, "Short MFT enumeration response");
            let next = u64::from_le_bytes(bytes[..8].try_into()?);
            ensure!(next > input.StartFileReferenceNumber, "MFT enumeration did not advance");
            for record in parse_records(&bytes[8..])? {
                consume(record)?;
                seen += 1;
                if max_records.is_some_and(|limit| seen >= limit) { return Ok(()); }
            }
            input.StartFileReferenceNumber = next;
        }
        Ok(())
    }

    pub fn read_changes<F>(&self, start_usn: i64, end_usn: i64, journal_id: u64, mut consume: F) -> Result<i64>
    where
        F: FnMut(UsnRecord) -> Result<()>,
    {
        ensure!(end_usn >= start_usn, "USN endpoint precedes scan start");
        let mut input = READ_USN_JOURNAL_DATA_V0 {
            StartUsn: start_usn,
            ReasonMask: u32::MAX,
            ReturnOnlyOnClose: 0,
            Timeout: 0,
            BytesToWaitFor: 0,
            UsnJournalID: journal_id,
        };
        let mut buffer = vec![0u8; BUFFER_SIZE];
        loop {
            if input.StartUsn >= end_usn { return Ok(input.StartUsn); }
            let mut written = 0;
            self.control(
                FSCTL_READ_USN_JOURNAL,
                Some((&input as *const READ_USN_JOURNAL_DATA_V0).cast()),
                Some(buffer.as_mut_ptr().cast()),
                buffer.len() as u32,
                &mut written,
            ).context("Read USN journal")?;
            let bytes = &buffer[..written as usize];
            ensure!(bytes.len() >= 8, "Short USN journal response");
            let next = i64::from_le_bytes(bytes[..8].try_into()?);
            ensure!(next >= input.StartUsn, "USN journal cursor moved backwards");
            if bytes.len() == 8 {
                ensure!(next >= end_usn, "USN journal stopped before the captured endpoint");
                return Ok(next);
            }
            ensure!(next > input.StartUsn, "USN journal did not advance");
            for record in parse_records(&bytes[8..])? { consume(record)?; }
            input.StartUsn = next;
        }
    }

    fn control(
        &self,
        code: u32,
        input: Option<*const c_void>,
        output: Option<*mut c_void>,
        output_len: u32,
        written: &mut u32,
    ) -> windows::core::Result<()> {
        let input_len = match code {
            FSCTL_ENUM_USN_DATA => size_of::<MFT_ENUM_DATA_V0>() as u32,
            FSCTL_READ_USN_JOURNAL => size_of::<READ_USN_JOURNAL_DATA_V0>() as u32,
            _ => 0,
        };
        // The borrowed File owns the handle for the entire synchronous call.
        let handle = HANDLE(self.file.as_raw_handle());
        unsafe { DeviceIoControl(handle, code, input, input_len, output, output_len, Some(written), None) }
    }
}

pub fn scan_volume(
    root: &str,
    store: &mut Store,
    cancelled: &dyn Fn() -> bool,
    progress: &mut dyn FnMut(ScanProgress),
    counters: &mut ScanCounters,
) -> Result<i64> {
    let letter = root.chars().next().context("Missing drive letter")?;
    ensure!(root.len() == 3 && root.as_bytes()[1..] == *b":\\", "Expected a drive root such as C:\\");
    let volume = Volume::open(letter)?;
    let mut reader = volume.ntfs_reader()?;
    let ntfs = Ntfs::new(&mut reader).context("Read NTFS boot sector")?;
    let root_id = root_id(&ntfs, &mut reader)?;
    let volume_id = format!("ntfs:{:016x}", ntfs.serial_number());
    let start = volume.journal()?;
    let previous = store.checkpoint(root)?;
    let (mode, full_reason) = scan_mode(previous.as_ref(), &volume_id, root_id, start);
    let mut pending: HashSet<u64> = if mode == ScanMode::Incremental {
        store.pending(root)?.into_iter().collect()
    } else { HashSet::new() };
    let mut load = CpuLoad::new([Some(letter), store.database_drive()])?;
    progress(ScanProgress {
        phase: if mode == ScanMode::Full { "MFT" } else { "USN" }.into(),
        processed: 0,
        expected: 0,
    });
    store.begin_stage()?;
    let mut processed = 0u64;
    let run = (|| -> Result<ScanOutcome> {
        std::thread::sleep(Duration::from_millis(500));
        let mut phase = if mode == ScanMode::Full { "MFT" } else { "USN" };
        load.wait_if_busy(cancelled, progress, phase, processed)?;
        if mode == ScanMode::Full {
            counters.entry_reads += 1;
            let root_entry = read_entry(&ntfs, &mut reader, root_id)?.context("Root directory disappeared")?;
            store.stage(Mutation::Upsert(root_entry))?;
            volume.enumerate(start.next_usn, None, |record| {
                if cancelled() { anyhow::bail!("Scan cancelled"); }
                if record.id != root_id && (record.id & 0x0000_ffff_ffff_ffff) >= 16 {
                    stage_record(&ntfs, &mut reader, store, &volume_id, record.id, true, &mut pending, &mut counters.entry_reads)?;
                }
                processed += 1;
                if processed % 512 == 0 {
                    std::thread::yield_now();
                    load.wait_if_busy(cancelled, progress, phase, processed)?;
                }
                if processed % 4096 == 0 {
                    progress(ScanProgress { phase: "MFT".into(), processed, expected: 0 });
                }
                Ok(())
            })?;
        }
        reader.invalidate();
        let endpoint = volume.journal()?;
        ensure!(endpoint.id == start.id && endpoint.first_usn <= start.next_usn,
            "USN journal reset during scan");
        let from = if mode == ScanMode::Full { start.next_usn } else { previous.as_ref().unwrap().next_usn };
        phase = "合并变化记录";
        let mut changed = std::collections::HashMap::new();
        let next_usn = volume.read_changes(from, endpoint.next_usn, start.id, |record| {
            if cancelled() { anyhow::bail!("Scan cancelled"); }
            if (record.id & 0x0000_ffff_ffff_ffff) < 16 { return Ok(()); }
            merge_change(&mut changed, &record);
            counters.journal_records += 1;
            processed += 1;
            if processed % 512 == 0 {
                std::thread::yield_now();
                load.wait_if_busy(cancelled, progress, phase, processed)?;
            }
            if processed % 4096 == 0 {
                progress(ScanProgress { phase: "合并变化记录".into(), processed, expected: 0 });
            }
            Ok(())
        })?;
        counters.unique_changed_files = changed.len() as u64;
        phase = "读取变化文件";
        progress(ScanProgress { phase: phase.into(), processed: 0, expected: changed.len() as u64 });
        let expected = changed.len() as u64;
        for (index, (id, deleted)) in changed.into_iter().enumerate() {
            if cancelled() { anyhow::bail!("Scan cancelled"); }
            // The complete reference number includes the MFT sequence, so reused slots stay distinct.
            if deleted {
                store.stage(Mutation::Delete(id))?;
                pending.remove(&id);
            } else {
                reader.invalidate();
                stage_record(&ntfs, &mut reader, store, &volume_id, id, false, &mut pending, &mut counters.entry_reads)?;
            }
            if index % 512 == 0 {
                load.wait_if_busy(cancelled, progress, phase, index as u64)?;
                progress(ScanProgress { phase: phase.into(), processed: index as u64 + 1, expected });
            }
        }
        for id in pending.iter().copied().collect::<Vec<_>>() {
            if cancelled() { anyhow::bail!("Scan cancelled"); }
            reader.invalidate();
            stage_record(&ntfs, &mut reader, store, &volume_id, id,
                mode == ScanMode::Full, &mut pending, &mut counters.entry_reads)?;
        }
        let mut tried_parents = HashSet::new();
        loop {
            let missing: Vec<_> = store.missing_parent_ids(&volume_id, mode == ScanMode::Full)?
                .into_iter().filter(|id| tried_parents.insert(*id)).collect();
            if missing.is_empty() { break; }
            for id in missing {
                load.wait_if_busy(cancelled, progress, "补全目录路径", processed)?;
                counters.entry_reads += 1;
                reader.invalidate();
                if let Ok(Some(entry)) = read_entry(&ntfs, &mut reader, id) {
                    if entry.is_dir { store.stage(Mutation::Upsert(entry))?; }
                }
            }
        }
        let after = volume.journal()?;
        ensure!(after.id == start.id && after.first_usn <= next_usn,
            "USN journal gap before commit");
        if cancelled() { anyhow::bail!("Scan cancelled"); }
        let (total, free) = disk_space(root)?;
        let mut pending: Vec<_> = pending.iter().copied().collect();
        pending.sort_unstable();
        let retry_warning = (!pending.is_empty()).then(|| format!(
            "{} 个 NTFS 文件记录的数据属性暂时无法读取，保留旧索引并在下次扫描重试（例如记录 {}）",
            pending.len(), pending[0] & 0x0000_ffff_ffff_ffff));
        Ok(ScanOutcome {
            checkpoint: Checkpoint { volume_id, journal_id: start.id, next_usn, root_id },
            mode, total, free, sampled_at: chrono::Local::now().to_rfc3339(), pending,
            warnings: full_reason.into_iter().map(str::to_owned)
                .chain(load.disk_warning().into_iter().map(str::to_owned))
                .chain(retry_warning).collect(),
        })
    })();
    match run {
        Ok(outcome) => {
            progress(ScanProgress { phase: "提交索引".into(), processed, expected: 0 });
            store.commit_stage_control(root, &outcome, &mut ||
                load.wait_if_busy(cancelled, progress, "提交索引", processed))
        }
        Err(error) => { store.discard_stage()?; Err(error) }
    }
}

fn missing_data_attribute(error: &anyhow::Error) -> bool {
    matches!(error.downcast_ref::<NtfsError>(),
        Some(NtfsError::AttributeNotFound { ty: NtfsAttributeType::Data, .. }))
}

fn merge_change(changed: &mut std::collections::HashMap<u64, bool>, record: &UsnRecord) {
    let deleted = record.reason & 0x0000_0200 != 0;
    changed.entry(record.id).and_modify(|value| *value |= deleted).or_insert(deleted);
}

fn stage_record(ntfs: &Ntfs, reader: &mut AlignedReader, store: &mut Store,
    volume: &str, id: u64, full: bool, pending: &mut HashSet<u64>, reads: &mut u64) -> Result<()> {
    *reads += 1;
    let mut result = read_entry(ntfs, reader, id);
    if result.as_ref().is_err_and(missing_data_attribute) {
        reader.invalidate();
        *reads += 1;
        result = read_entry(ntfs, reader, id);
    }
    match result {
        Ok(Some(entry)) => {
            store.stage(Mutation::Upsert(entry))?;
            pending.remove(&id);
        }
        Ok(None) => {
            store.stage(Mutation::Delete(id))?;
            pending.remove(&id);
        }
        Err(error) if missing_data_attribute(&error) => {
            pending.insert(id);
            if full {
                if let Some(previous) = store.indexed_entry(volume, id)? {
                    store.stage(Mutation::Upsert(previous))?;
                }
            }
        }
        Err(error) => return Err(error).with_context(|| format!("Read NTFS record {}", id & 0x0000_ffff_ffff_ffff)),
    }
    Ok(())
}

fn scan_mode(previous: Option<&Checkpoint>, volume_id: &str, root_id: u64, journal: Journal) -> (ScanMode, Option<&'static str>) {
    let Some(previous) = previous else { return (ScanMode::Full, None); };
    let reason = if previous.volume_id != volume_id || previous.root_id != root_id {
        Some("磁盘身份已改变，重新建立完整基准")
    } else if previous.journal_id != journal.id {
        Some("USN 日志已重置，重新建立完整基准")
    } else if previous.next_usn < journal.first_usn {
        Some("USN 日志已覆盖上次扫描位置，重新建立完整基准")
    } else if previous.next_usn > journal.next_usn {
        Some("USN 检查点超出当前日志范围，重新建立完整基准")
    } else {
        None
    };
    if reason.is_some() { (ScanMode::Full, reason) } else { (ScanMode::Incremental, None) }
}

struct CpuLoad {
    previous: CpuTimes,
    checked_at: Instant,
    disk: Option<DiskLoad>,
    disk_failed: bool,
}

struct DiskLoad {
    query: PDH_HQUERY,
    counters: Vec<PDH_HCOUNTER>,
}

impl DiskLoad {
    fn new(letters: [Option<char>; 2]) -> Option<Self> {
        let mut query = PDH_HQUERY(std::ptr::null_mut());
        if unsafe { PdhOpenQueryW(windows::core::PCWSTR::null(), 0, &mut query) } != 0 { return None; }
        let mut load = Self { query, counters: Vec::new() };
        let mut seen = Vec::new();
        for letter in letters.into_iter().flatten() {
            let letter = letter.to_ascii_uppercase();
            if seen.contains(&letter) { continue; }
            seen.push(letter);
            let path: Vec<u16> = format!("\\LogicalDisk({letter}:)\\% Disk Time\0").encode_utf16().collect();
            let mut counter = PDH_HCOUNTER(std::ptr::null_mut());
            if unsafe { PdhAddEnglishCounterW(load.query, windows::core::PCWSTR(path.as_ptr()), 0, &mut counter) } != 0 {
                return None;
            }
            load.counters.push(counter);
        }
        if load.counters.is_empty() || unsafe { PdhCollectQueryData(load.query) } != 0 { return None; }
        Some(load)
    }

    fn busy_percent(&self) -> Option<f64> {
        if unsafe { PdhCollectQueryData(self.query) } != 0 { return None; }
        let mut maximum: f64 = 0.0;
        for counter in &self.counters {
            let mut value = PDH_FMT_COUNTERVALUE::default();
            let status = unsafe { PdhGetFormattedCounterValue(*counter, PDH_FMT_DOUBLE, None, &mut value) };
            if status != 0 || value.CStatus > 1 { return None; }
            maximum = maximum.max(unsafe { value.Anonymous.doubleValue });
        }
        Some(maximum)
    }
}

impl Drop for DiskLoad {
    fn drop(&mut self) { unsafe { PdhCloseQuery(self.query); } }
}

#[derive(Clone, Copy)]
struct CpuTimes { idle: u64, kernel: u64, user: u64 }

impl CpuTimes {
    fn read() -> Result<Self> {
        let mut idle = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        unsafe { GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user))?; }
        let ticks = |value: FILETIME| (u64::from(value.dwHighDateTime) << 32) | u64::from(value.dwLowDateTime);
        Ok(Self { idle: ticks(idle), kernel: ticks(kernel), user: ticks(user) })
    }

    fn busy_percent_since(self, previous: Self) -> f64 {
        let total = self.kernel.saturating_sub(previous.kernel)
            + self.user.saturating_sub(previous.user);
        let idle = self.idle.saturating_sub(previous.idle);
        if total == 0 { 0.0 } else { total.saturating_sub(idle) as f64 * 100.0 / total as f64 }
    }
}

impl CpuLoad {
    fn new(letters: [Option<char>; 2]) -> Result<Self> {
        let disk = DiskLoad::new(letters);
        Ok(Self { previous: CpuTimes::read()?, checked_at: Instant::now(),
            disk_failed: disk.is_none(), disk })
    }

    fn disk_warning(&self) -> Option<&'static str> {
        self.disk_failed.then_some("磁盘负载计数器不可用；仅按 CPU 负载暂停扫描")
    }

    fn wait_if_busy(
        &mut self, cancelled: &dyn Fn() -> bool, progress: &mut dyn FnMut(ScanProgress),
        phase: &str, processed: u64,
    ) -> Result<()> {
        let mut paused = false;
        let mut quiet = 0;
        loop {
            if cancelled() { anyhow::bail!("Scan cancelled"); }
            if self.checked_at.elapsed() < Duration::from_secs(2) { break; }
            let current = CpuTimes::read()?;
            let cpu = current.busy_percent_since(self.previous);
            let disk = self.disk.as_ref().and_then(DiskLoad::busy_percent);
            if self.disk.is_some() && disk.is_none() {
                self.disk = None;
                self.disk_failed = true;
            }
            self.previous = current;
            self.checked_at = Instant::now();
            if !paused && cpu <= 85.0 && disk.unwrap_or(0.0) <= 90.0 { break; }
            if paused && cpu < 75.0 && disk.unwrap_or(0.0) < 70.0 { quiet += 1; }
            else { quiet = 0; }
            if paused && quiet >= 2 { break; }
            paused = true;
            progress(ScanProgress { phase: "系统繁忙，已暂停".into(), processed, expected: 0 });
            for _ in 0..10 {
                if cancelled() { anyhow::bail!("Scan cancelled"); }
                std::thread::sleep(Duration::from_millis(200));
            }
        }
        if paused { progress(ScanProgress { phase: phase.into(), processed, expected: 0 }); }
        Ok(())
    }
}

fn disk_space(root: &str) -> Result<(u64, u64)> {
    let wide: Vec<u16> = root.encode_utf16().chain(std::iter::once(0)).collect();
    let mut total = 0;
    let mut free = 0;
    unsafe {
        GetDiskFreeSpaceExW(
            windows::core::PCWSTR(wide.as_ptr()), None, Some(&mut total), Some(&mut free),
        )?;
    }
    Ok((total, free))
}

pub struct AlignedReader {
    file: File,
    position: u64,
    buffer: Vec<u8>,
    buffer_start: Option<u64>,
}

impl AlignedReader {
    fn new(file: File) -> Self {
        Self { file, position: 0, buffer: Vec::new(), buffer_start: None }
    }

    fn invalidate(&mut self) {
        self.buffer_start = None;
    }
}

impl Read for AlignedReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() { return Ok(0); }
        const SECTOR: u64 = 4096;
        let aligned = self.position / SECTOR * SECTOR;
        let start = (self.position - aligned) as usize;
        let needed = start.checked_add(out.len()).ok_or(io::ErrorKind::InvalidInput)?;
        let count = needed.checked_add(SECTOR as usize - 1)
            .ok_or(io::ErrorKind::InvalidInput)? / SECTOR as usize * SECTOR as usize;
        if self.buffer_start != Some(aligned) || self.buffer.len() < count {
            self.buffer_start = None;
            self.buffer.resize(count, 0);
            self.file.seek(SeekFrom::Start(aligned))?;
            self.file.read_exact(&mut self.buffer)?;
            self.buffer_start = Some(aligned);
        }
        out.copy_from_slice(&self.buffer[start..needed]);
        self.position += out.len() as u64;
        Ok(out.len())
    }
}

impl Seek for AlignedReader {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        self.position = match from {
            SeekFrom::Start(position) => position,
            SeekFrom::Current(delta) => self.position.checked_add_signed(delta)
                .ok_or(io::ErrorKind::InvalidInput)?,
            SeekFrom::End(_) => return Err(io::ErrorKind::Unsupported.into()),
        };
        Ok(self.position)
    }
}

pub fn root_id(ntfs: &Ntfs, reader: &mut AlignedReader) -> Result<u64> {
    let file = ntfs.root_directory(reader)?;
    Ok(file.file_record_number() | (u64::from(file.sequence_number()) << 48))
}

pub fn read_entry(ntfs: &Ntfs, reader: &mut AlignedReader, id: u64) -> Result<Option<Entry>> {
    let record_number = id & 0x0000_ffff_ffff_ffff;
    let file = ntfs.file(reader, record_number)
        .with_context(|| format!("Read NTFS file record {record_number}"))?;
    if !file.flags().contains(ntfs::NtfsFileFlags::IN_USE)
        || file.sequence_number() != (id >> 48) as u16
    { return Ok(None); }
    entry_from_file(&file, reader, id).map(Some)
}

fn entry_from_file(file: &NtfsFile, reader: &mut AlignedReader, id: u64) -> Result<Entry> {
    let info = file.info()?;
    let mut names = Vec::<(u8, Link)>::new();
    let mut logical = 0;
    let mut allocated = 0;
    let mut attributes = file.attributes();
    while let Some(item) = attributes.next(reader) {
        let item = item?;
        let attribute = item.to_attribute()?;
        match attribute.ty()? {
            NtfsAttributeType::FileName => {
                let name = attribute.structured_value::<_, NtfsFileName>(reader)?;
                let rank = match name.namespace() {
                    NtfsFileNamespace::Dos => continue,
                    NtfsFileNamespace::Win32 => 0,
                    NtfsFileNamespace::Win32AndDos => 1,
                    NtfsFileNamespace::Posix => 2,
                };
                let parent = name.parent_directory_reference();
                names.push((rank, Link {
                    parent: parent.file_record_number() | (u64::from(parent.sequence_number()) << 48),
                    name: name.name().to_string_lossy(),
                }));
            }
            NtfsAttributeType::Data if !file.is_directory() => {
                if attribute.name()?.is_empty() { logical = attribute.value_length(); }
                match attribute.value(reader)? {
                    NtfsAttributeValue::Resident(_) => {}
                    NtfsAttributeValue::NonResident(value) => {
                        for run in value.data_runs() {
                            let run = run?;
                            if run.data_position().value().is_some() {
                                allocated += run.allocated_size();
                            }
                        }
                    }
                    NtfsAttributeValue::AttributeListNonResident(_) => {
                        allocated += split_stream_allocated(file, reader, attribute.instance())?;
                    }
                }
            }
            _ => {}
        }
    }
    names.sort_by(|(ra, a), (rb, b)| ra.cmp(rb).then_with(|| a.parent.cmp(&b.parent)).then_with(|| a.name.cmp(&b.name)));
    names.dedup_by(|(_, a), (_, b)| a == b);
    let links: Vec<Link> = names.into_iter().map(|(_, link)| link).collect();
    let first = links.first();
    let (parent, name) = match first {
        Some(link) => (link.parent, link.name.clone()),
        None if file.file_record_number() == 5 => (id, String::new()),
        None => anyhow::bail!("No usable filename for file record {}", file.file_record_number()),
    };
    let modified = info.modification_time().nt_timestamp() as i64 / 10_000_000 - 11_644_473_600;
    Ok(Entry {
        id, parent, name, is_dir: file.is_directory(), logical, allocated, modified,
        attributes: info.file_attributes().bits(), links,
    })
}

fn split_stream_allocated(file: &NtfsFile, reader: &mut AlignedReader, instance: u16) -> Result<u64> {
    let mut allocated = 0;
    let mut found = false;
    for raw in file.attributes_raw() {
        let raw = raw?;
        if raw.ty()? != NtfsAttributeType::AttributeList { continue; }
        let list = raw.structured_value::<_, NtfsAttributeList>(reader)?;
        let mut entries = list.entries();
        while let Some(entry) = entries.next(reader) {
            let entry = entry?;
            if entry.ty()? != NtfsAttributeType::Data || entry.instance() != instance { continue; }
            let segment_file = entry.to_file(file.ntfs(), reader)?;
            let segment = entry.to_attribute(&segment_file)?;
            match segment.value(reader)? {
                NtfsAttributeValue::NonResident(value) => {
                    for run in value.data_runs() {
                        let run = run?;
                        if run.data_position().value().is_some() {
                            allocated += run.allocated_size();
                        }
                    }
                }
                _ => anyhow::bail!("Unexpected resident segment in file record {}", file.file_record_number()),
            }
            found = true;
        }
    }
    ensure!(found, "Missing split data stream in file record {}", file.file_record_number());
    Ok(allocated)
}

fn parse_records(mut bytes: &[u8]) -> Result<Vec<UsnRecord>> {
    let mut result = Vec::new();
    while !bytes.is_empty() {
        ensure!(bytes.len() >= RECORD_HEADER_SIZE, "Truncated USN record header");
        let length = u32::from_le_bytes(bytes[..4].try_into()?) as usize;
        ensure!(length >= RECORD_HEADER_SIZE && length <= bytes.len(), "Invalid USN record length");
        let major = u16::from_le_bytes(bytes[4..6].try_into()?);
        ensure!(major == 2, "Unsupported USN record version {major}");
        let record = &bytes[..length];
        let name_len = u16::from_le_bytes(record[56..58].try_into()?) as usize;
        let name_start = u16::from_le_bytes(record[58..60].try_into()?) as usize;
        ensure!(name_len % 2 == 0 && name_start >= RECORD_HEADER_SIZE && name_start.checked_add(name_len).is_some_and(|end| end <= length), "Invalid USN filename range");
        let name = String::from_utf16(
            &record[name_start..name_start + name_len]
                .chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .collect::<Vec<_>>(),
        ).context("Invalid UTF-16 filename in USN record")?;
        result.push(UsnRecord {
            id: u64::from_le_bytes(record[8..16].try_into()?),
            parent: u64::from_le_bytes(record[16..24].try_into()?),
            usn: i64::from_le_bytes(record[24..32].try_into()?),
            reason: u32::from_le_bytes(record[40..44].try_into()?),
            attributes: u32::from_le_bytes(record[52..56].try_into()?),
            name,
        });
        bytes = &bytes[length..];
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs::OpenOptions, io::Write};

    #[test]
    fn journal_events_merge_by_complete_file_identity_and_keep_deletions() {
        let mut changed = std::collections::HashMap::new();
        let record = |id, reason| UsnRecord { id, reason, parent: 5, usn: 1, attributes: 0, name: "file".into() };
        let original = (7u64 << 48) | 42;
        let reused = (8u64 << 48) | 42;
        for _ in 0..10000 { merge_change(&mut changed, &record(original, 1)); }
        assert_eq!(changed.len(), 1);
        merge_change(&mut changed, &record(original, 0x200));
        merge_change(&mut changed, &record(original, 0x8000_0000));
        merge_change(&mut changed, &record(reused, 0x100));
        assert_eq!(changed.len(), 2);
        assert_eq!(changed[&original], true);
        assert_eq!(changed[&reused], false);
    }

    #[test]
    fn usn_v2_record_rejects_truncation_and_decodes_unicode() {
        let mut bytes = vec![0u8; 64];
        bytes[..4].copy_from_slice(&64u32.to_le_bytes());
        bytes[4..6].copy_from_slice(&2u16.to_le_bytes());
        bytes[8..16].copy_from_slice(&42u64.to_le_bytes());
        bytes[16..24].copy_from_slice(&5u64.to_le_bytes());
        bytes[56..58].copy_from_slice(&4u16.to_le_bytes());
        bytes[58..60].copy_from_slice(&60u16.to_le_bytes());
        bytes[60..64].copy_from_slice(&[0x2d, 0x4e, 0x87, 0x65]);
        let records = parse_records(&bytes).unwrap();
        assert_eq!(records[0].name, "中文");
        assert_eq!(records[0].id, 42);
        assert!(parse_records(&bytes[..63]).is_err());
        bytes[4..6].copy_from_slice(&3u16.to_le_bytes());
        assert!(parse_records(&bytes).is_err());
    }

    #[test]
    fn aligned_reader_reuses_sector_until_invalidated() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("volume.bin");
        std::fs::write(&path, vec![1u8; 4096]).unwrap();
        let mut reader = AlignedReader::new(File::open(&path).unwrap());
        let mut value = [0; 4];
        reader.seek(SeekFrom::Start(100)).unwrap();
        reader.read_exact(&mut value).unwrap();
        assert_eq!(value, [1; 4]);

        OpenOptions::new().write(true).open(&path).unwrap().write_all(&[2; 4]).unwrap();
        reader.seek(SeekFrom::Start(0)).unwrap();
        reader.read_exact(&mut value).unwrap();
        assert_eq!(value, [1; 4]);

        reader.invalidate();
        reader.seek(SeekFrom::Start(0)).unwrap();
        reader.read_exact(&mut value).unwrap();
        assert_eq!(value, [2; 4]);
    }

    #[test]
    fn cpu_load_threshold_uses_idle_time() {
        let before = CpuTimes { idle: 100, kernel: 200, user: 100 };
        assert!(CpuTimes { idle: 180, kernel: 300, user: 100 }.busy_percent_since(before) < 75.0);
        assert!(CpuTimes { idle: 110, kernel: 300, user: 100 }.busy_percent_since(before) > 85.0);
    }

    #[test]
    fn scan_mode_explains_journal_gap() {
        let checkpoint = Checkpoint {
            volume_id: "ntfs:test".into(), journal_id: 7, next_usn: 50, root_id: 5,
        };
        let journal = Journal { id: 7, first_usn: 60, next_usn: 100 };
        let (mode, reason) = scan_mode(Some(&checkpoint), "ntfs:test", 5, journal);
        assert_eq!(mode, ScanMode::Full);
        assert!(reason.unwrap().contains("覆盖"));
        let (mode, reason) = scan_mode(Some(&checkpoint), "ntfs:test", 5,
            Journal { first_usn: 40, ..journal });
        assert_eq!(mode, ScanMode::Incremental);
        assert!(reason.is_none());
    }

}
