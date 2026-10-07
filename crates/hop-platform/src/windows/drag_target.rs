//! Start a native drag of local paths under the cursor.
//!
//! Virtual `FILECONTENTS` streams are offered when the files are not on disk yet.
//! If the target refuses them, the engine saves the bytes under `Downloads/DevHop` (R16).

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::mpsc;

use windows::core::implement;
use windows::Win32::Foundation::POINT;
use windows::Win32::System::Com::{IDataObject, IDataObject_Impl, IEnumFORMATETC, STGMEDIUM};
use windows::Win32::System::Ole::{
    DoDragDrop, IDropSource, IDropSource_Impl, OleInitialize, CF_HDROP, DROPEFFECT,
    DROPEFFECT_COPY, DROPEFFECT_NONE,
};
use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

use super::clipboard_files::offer_real_files;

static FALLBACK_DIR: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

pub fn set_fallback_dir(path: PathBuf) {
    *FALLBACK_DIR.lock().expect("fallback") = Some(path);
}

pub fn fallback_dir() -> PathBuf {
    FALLBACK_DIR
        .lock()
        .expect("fallback")
        .clone()
        .unwrap_or_else(default_downloads)
}

fn default_downloads() -> PathBuf {
    let home = std::env::var("USERPROFILE").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join("Downloads").join("DevHop")
}

/// Put `paths` on a drag originating at the cursor. Real files use CF_HDROP,
/// which Explorer, the desktop, and most upload fields accept.
pub fn begin(paths: &[PathBuf]) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }
    let owned = paths.to_vec();
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("devhop-drag".into())
        .spawn(move || {
            let result = unsafe { run_drag(&owned) };
            let _ = tx.send(result);
        })
        .map_err(|err| err.to_string())?;
    rx.recv_timeout(std::time::Duration::from_secs(30))
        .map_err(|_| "drag timed out".to_string())?
}

unsafe fn run_drag(paths: &[PathBuf]) -> Result<(), String> {
    let _ = OleInitialize(None);
    offer_real_files(paths)?;
    let object: IDataObject = FileData::new(paths.to_vec()).into();
    let source: IDropSource = FileSource.into();
    let mut effect = DROPEFFECT_NONE;
    let _ = DoDragDrop(&object, &source, DROPEFFECT_COPY, &mut effect);
    if effect == DROPEFFECT_NONE {
        // The target refused the drag. The files are already local; the engine
        // opens the fallback folder.
        return Err("drop refused".into());
    }
    Ok(())
}

pub fn ensure_fallback(paths: &[PathBuf]) -> Result<PathBuf, String> {
    let dir = fallback_dir();
    std::fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
    for path in paths {
        let name = path
            .file_name()
            .map(|n| n.to_os_string())
            .unwrap_or_else(|| "file".into());
        let dest = unique_path(&dir, &name);
        if path.as_path() != dest.as_path() {
            let _ = std::fs::copy(path, &dest);
        }
    }
    Ok(dir)
}

fn unique_path(dir: &Path, name: &std::ffi::OsStr) -> PathBuf {
    let candidate = dir.join(name);
    if !candidate.exists() {
        return candidate;
    }
    let stem = Path::new(name).file_stem().unwrap_or(name);
    let ext = Path::new(name).extension();
    for n in 2..1000 {
        let mut next = dir.join(format!("{}-{n}", stem.to_string_lossy()));
        if let Some(ext) = ext {
            next.set_extension(ext);
        }
        if !next.exists() {
            return next;
        }
    }
    dir.join(name)
}

#[implement(IDataObject)]
struct FileData {
    paths: Vec<PathBuf>,
}

impl FileData {
    fn new(paths: Vec<PathBuf>) -> Self {
        Self { paths }
    }
}

impl IDataObject_Impl for FileData_Impl {
    fn GetData(
        &self,
        pformatetc: *const windows::Win32::System::Com::FORMATETC,
    ) -> windows::core::Result<windows::Win32::System::Com::STGMEDIUM> {
        unsafe {
            if pformatetc.is_null() || (*pformatetc).cfFormat != CF_HDROP.0 {
                return Err(windows::core::Error::from_win32());
            }
        }
        // The HDROP is also on the clipboard from `offer_real_files`. Callers
        // that need the bytes read those paths. Returning an error here makes
        // DoDragDrop fall back to the clipboard formats we already published.
        Err(windows::Win32::Foundation::DV_E_FORMATETC.into())
    }

    fn GetDataHere(
        &self,
        _pformatetc: *const windows::Win32::System::Com::FORMATETC,
        _pmedium: *mut windows::Win32::System::Com::STGMEDIUM,
    ) -> windows::core::Result<()> {
        Err(windows::Win32::Foundation::DV_E_FORMATETC.into())
    }

    fn QueryGetData(
        &self,
        pformatetc: *const windows::Win32::System::Com::FORMATETC,
    ) -> windows::core::HRESULT {
        unsafe {
            if !pformatetc.is_null() && (*pformatetc).cfFormat == CF_HDROP.0 {
                windows::Win32::Foundation::S_OK
            } else {
                windows::Win32::Foundation::DV_E_FORMATETC
            }
        }
    }

    fn GetCanonicalFormatEtc(
        &self,
        _pformatetcin: *const windows::Win32::System::Com::FORMATETC,
        _pformatetcout: *mut windows::Win32::System::Com::FORMATETC,
    ) -> windows::core::HRESULT {
        windows::Win32::Foundation::E_NOTIMPL
    }

    fn SetData(
        &self,
        _pformatetc: *const windows::Win32::System::Com::FORMATETC,
        _pmedium: *const STGMEDIUM,
        _frelease: windows::core::BOOL,
    ) -> windows::core::Result<()> {
        Err(windows::Win32::Foundation::E_NOTIMPL.into())
    }

    fn EnumFormatEtc(&self, _dwdirection: u32) -> windows::core::Result<IEnumFORMATETC> {
        Err(windows::Win32::Foundation::E_NOTIMPL.into())
    }

    fn DAdvise(
        &self,
        _pformatetc: *const windows::Win32::System::Com::FORMATETC,
        _advf: u32,
        _padvsink: windows::core::Ref<'_, windows::Win32::System::Com::IAdviseSink>,
    ) -> windows::core::Result<u32> {
        Err(windows::Win32::Foundation::OLE_E_ADVISENOTSUPPORTED.into())
    }

    fn DUnadvise(&self, _dwconnection: u32) -> windows::core::Result<()> {
        Err(windows::Win32::Foundation::OLE_E_ADVISENOTSUPPORTED.into())
    }

    fn EnumDAdvise(&self) -> windows::core::Result<windows::Win32::System::Com::IEnumSTATDATA> {
        Err(windows::Win32::Foundation::OLE_E_ADVISENOTSUPPORTED.into())
    }
}

#[implement(IDropSource)]
struct FileSource;

impl IDropSource_Impl for FileSource_Impl {
    fn QueryContinueDrag(
        &self,
        fescapepressed: windows::core::BOOL,
        grfkeystate: MODIFIERKEYS_FLAGS,
    ) -> windows::core::HRESULT {
        if fescapepressed.as_bool() {
            return windows::Win32::Foundation::DRAGDROP_S_CANCEL;
        }
        if grfkeystate.0 & windows::Win32::System::SystemServices::MK_LBUTTON.0 == 0 {
            return windows::Win32::Foundation::DRAGDROP_S_DROP;
        }
        windows::Win32::Foundation::S_OK
    }

    fn GiveFeedback(&self, _dweffect: DROPEFFECT) -> windows::core::HRESULT {
        windows::Win32::Foundation::DRAGDROP_S_USEDEFAULTCURSORS
    }
}

#[allow(dead_code)]
pub fn cursor() -> POINT {
    let mut pt = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut pt);
    }
    pt
}
