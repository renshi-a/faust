//! A tiny "SDK" made only of `.tbd` text stubs, so Rust can link Mac apps without Xcode.
//!
//! Stage 1 links against [`write_stubs`]: an empty stub for every system library and
//! framework, with `-undefined dynamic_lookup`. [`write_mini_sdk`] then reads what the stage-1
//! binaries import, asks the running OS (dlsym and dladdr) which library defines each
//! symbol, and writes stubs that list those symbols, so stage 2 is an ordinary two-level
//! namespace link against them.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use object::read::macho::MachOFile64;
use object::{Endianness, Object, ObjectSymbol};

/// Libraries in `/usr/lib` that Rust code links, with the names they are linked by. rustc
/// itself asks for `-lSystem -lc -lm`; the `libc` crate adds `-liconv`, objc2 `-lobjc`.
const DYLIBS: &[(&str, &[&str])] = &[
    ("/usr/lib/libSystem.B.dylib", &["System", "System.B", "c", "m", "pthread", "dl", "info", "rpcsvc", "proc"]),
    ("/usr/lib/libobjc.A.dylib", &["objc"]),
    ("/usr/lib/libiconv.2.dylib", &["iconv"]),
    ("/usr/lib/libresolv.9.dylib", &["resolv"]),
    ("/usr/lib/libz.1.dylib", &["z"]),
    ("/usr/lib/libc++.1.dylib", &["c++"]),
    ("/usr/lib/libsqlite3.dylib", &["sqlite3"]),
    ("/usr/lib/libcompression.dylib", &["compression"]),
];

const FRAMEWORKS: &str = "/System/Library/Frameworks";
const LIBSYSTEM: &str = "/usr/lib/libSystem.B.dylib";
const LIBOBJC: &str = "/usr/lib/libobjc.A.dylib";

/// One stub file: where it sits in the SDK, and the library it stands for.
#[derive(Debug, Clone)]
struct Stub {
    rel: PathBuf,
    install_name: String,
}

/// Every stub the SDK holds: the dylibs above under each of their names, and one per
/// public framework.
fn stubs() -> Vec<Stub> {
    let mut out = Vec::new();
    for (install_name, names) in DYLIBS {
        for name in *names {
            out.push(Stub {
                rel: PathBuf::from(format!("usr/lib/lib{name}.tbd")),
                install_name: (*install_name).to_owned(),
            });
        }
    }
    let mut frameworks: Vec<String> = std::fs::read_dir(FRAMEWORKS)
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|e| e.file_name().to_str()?.strip_suffix(".framework").map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    frameworks.sort();
    for name in frameworks {
        out.push(Stub {
            rel: PathBuf::from(format!("System/Library/Frameworks/{name}.framework/{name}.tbd")),
            install_name: framework_install_name(&name),
        });
    }
    out
}

/// `/System/Library/Frameworks/AppKit.framework/Versions/C/AppKit`: the version folder is
/// whatever `Versions/Current` points at.
fn framework_install_name(name: &str) -> String {
    let dir = Path::new(FRAMEWORKS).join(format!("{name}.framework"));
    match std::fs::read_link(dir.join("Versions/Current")) {
        Ok(version) => format!("{}/Versions/{}/{name}", dir.display(), version.display()),
        Err(_) => format!("{}/{name}", dir.display()),
    }
}

/// Writes the stage-1 SDK: every stub, empty.
pub(crate) fn write_stubs(dir: &Path) -> Result<()> {
    let _ = std::fs::remove_dir_all(dir);
    let empty = Exports::default();
    for stub in stubs() {
        write_file(&dir.join(&stub.rel), &render_tbd(&stub.install_name, &empty))?;
    }
    write_settings(dir)
}

/// What the stage-2 link needs besides the SDK: stubs for libraries the code reaches
/// although nothing on the link line names them (passed to the linker as files).
#[derive(Debug, Default)]
pub(crate) struct MiniSdk {
    pub(crate) extra_libraries: Vec<PathBuf>,
}

/// Writes the stage-2 SDK for what `binaries` (stage-1 output) import.
///
/// # Errors
///
/// When a binary can't be read, or a symbol is defined nowhere on this Mac.
pub(crate) fn write_mini_sdk(dir: &Path, binaries: &[PathBuf]) -> Result<MiniSdk> {
    let mut wanted = BTreeSet::new();
    let mut linked = BTreeSet::new();
    for path in binaries {
        let data = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        let file = MachOFile64::<Endianness>::parse(&*data).with_context(|| format!("parsing {}", path.display()))?;
        for sym in file.symbols() {
            if sym.is_undefined() && sym.is_global() {
                wanted.insert(sym.name()?.to_owned());
            }
        }
        let endian = file.endian();
        let mut commands = file.macho_load_commands()?;
        while let Some(command) = commands.next()? {
            if let Some(dylib) = command.dylib()? {
                linked.insert(String::from_utf8_lossy(command.string(endian, dylib.dylib.name)?).into_owned());
            }
        }
    }
    // The linker refers to this itself for lazy binding below macOS 12 (no chained fixups).
    wanted.insert("dyld_stub_binder".to_owned());

    // Load what the binaries link, so RTLD_DEFAULT lookups see their symbols.
    for lib in &linked {
        let path = CString::new(lib.as_str())?;
        // SAFETY: dlopen takes a NUL-terminated path; a library that fails to load returns null.
        unsafe { dlopen(path.as_ptr(), RTLD_NOW) };
    }

    let mut by_library: BTreeMap<String, Exports> = BTreeMap::new();
    let mut unresolved = Vec::new();
    for mangled in &wanted {
        // dyld does not hand this one out through dlsym, but libSystem's public stub lists it.
        if mangled == "dyld_stub_binder" {
            by_library.entry(LIBSYSTEM.to_owned()).or_default().symbols.insert(mangled.clone());
            continue;
        }
        let image = match defining_image(mangled) {
            Ok(image) => image,
            Err(why) => {
                unresolved.push(format!("{mangled} ({why})"));
                continue;
            }
        };
        let library = library_for(&image, &linked);
        by_library.entry(library).or_default().add(mangled);
    }
    if !unresolved.is_empty() {
        bail!("these symbols are defined nowhere on this Mac:\n  {}", unresolved.join("\n  "));
    }

    let _ = std::fs::remove_dir_all(dir);
    let empty = Exports::default();
    let all = stubs();
    let in_sdk: BTreeSet<&str> = all.iter().map(|s| s.install_name.as_str()).collect();
    for stub in &all {
        let exports = by_library.get(&stub.install_name).unwrap_or(&empty);
        write_file(&dir.join(&stub.rel), &render_tbd(&stub.install_name, exports))?;
    }
    let mut sdk = MiniSdk::default();
    for (library, exports) in &by_library {
        if in_sdk.contains(library.as_str()) {
            continue;
        }
        let file = dir.join("extra").join(format!("{}.tbd", library.trim_start_matches('/').replace('/', "_")));
        write_file(&file, &render_tbd(library, exports))?;
        sdk.extra_libraries.push(file);
    }
    write_settings(dir)?;
    Ok(sdk)
}

/// The image (dylib or framework binary) that defines a Mach-O symbol on this Mac.
fn defining_image(mangled: &str) -> std::result::Result<String, &'static str> {
    // C symbols carry a leading underscore in Mach-O; dlsym wants it removed.
    let name = mangled.strip_prefix('_').ok_or("no leading underscore")?;
    let name = CString::new(name).map_err(|_| "NUL in name")?;
    // SAFETY: dlsym takes a handle and a NUL-terminated name and returns null when not found.
    let addr = unsafe { dlsym(RTLD_DEFAULT, name.as_ptr()) };
    if addr.is_null() {
        return Err("dlsym found nothing");
    }
    let mut info = DlInfo {
        fname: std::ptr::null(),
        fbase: std::ptr::null_mut(),
        sname: std::ptr::null(),
        saddr: std::ptr::null_mut(),
    };
    // SAFETY: `addr` came from dlsym, and `info` is a valid, writable Dl_info.
    if unsafe { dladdr(addr, &mut info) } == 0 || info.fname.is_null() {
        return Err("dladdr found nothing");
    }
    // SAFETY: dladdr filled `fname` with a NUL-terminated path that lives as long as the image.
    Ok(unsafe { CStr::from_ptr(info.fname) }.to_string_lossy().into_owned())
}

/// The library a binary should bind a symbol to, given the image that defines it and the
/// libraries the binary links. Images re-exported by an umbrella (libSystem's parts, a
/// framework's sub-frameworks) bind to the umbrella, which re-exports them at run time too.
fn library_for(image: &str, linked: &BTreeSet<String>) -> String {
    if image == LIBSYSTEM || image.starts_with("/usr/lib/system/") || image == "/usr/lib/dyld" {
        return LIBSYSTEM.to_owned();
    }
    if image.starts_with("/usr/lib/libobjc") || image.starts_with("/usr/lib/objc/") {
        return LIBOBJC.to_owned();
    }
    if linked.contains(image) {
        return image.to_owned();
    }
    // /System/Library/Frameworks/X.framework/Versions/V/Frameworks/Y.framework/... -> X
    if let Some(rest) = image.strip_prefix("/System/Library/Frameworks/")
        && let Some((umbrella, inner)) = rest.split_once(".framework/")
        && inner.contains("/Frameworks/")
    {
        let umbrella = framework_install_name(umbrella);
        if linked.contains(&umbrella) {
            return umbrella;
        }
    }
    // CoreFoundation is re-exported by Foundation.
    if image.starts_with("/System/Library/Frameworks/CoreFoundation.framework/") {
        let foundation = framework_install_name("Foundation");
        if linked.contains(&foundation) && !linked.contains(image) {
            return foundation;
        }
    }
    image.to_owned()
}

#[derive(Debug, Default)]
struct Exports {
    symbols: BTreeSet<String>,
    objc_classes: BTreeSet<String>,
    objc_eh_types: BTreeSet<String>,
}

impl Exports {
    fn add(&mut self, mangled: &str) {
        if let Some(class) =
            mangled.strip_prefix("_OBJC_CLASS_$_").or_else(|| mangled.strip_prefix("_OBJC_METACLASS_$_"))
        {
            self.objc_classes.insert(class.to_owned());
        } else if let Some(class) = mangled.strip_prefix("_OBJC_EHTYPE_$_") {
            self.objc_eh_types.insert(class.to_owned());
        } else {
            self.symbols.insert(mangled.to_owned());
        }
    }

    fn is_empty(&self) -> bool {
        self.symbols.is_empty() && self.objc_classes.is_empty() && self.objc_eh_types.is_empty()
    }
}

fn render_tbd(install_name: &str, e: &Exports) -> String {
    fn list(items: &BTreeSet<String>) -> String {
        items.iter().map(|s| format!("'{s}'")).collect::<Vec<_>>().join(", ")
    }
    let mut out = format!(
        "--- !tapi-tbd\ntbd-version:     4\ntargets:         [ arm64-macos ]\ninstall-name:    '{install_name}'\n"
    );
    if !e.is_empty() {
        out.push_str("exports:\n  - targets:         [ arm64-macos ]\n");
        for (key, items) in
            [("symbols", &e.symbols), ("objc-classes", &e.objc_classes), ("objc-eh-types", &e.objc_eh_types)]
        {
            if !items.is_empty() {
                let _ = writeln!(out, "    {key}:{:pad$}[ {} ]", "", list(items), pad = 16 - key.len());
            }
        }
    }
    out.push_str("...\n");
    out
}

/// rustc reads the SDK's version from here.
fn write_settings(dir: &Path) -> Result<()> {
    write_file(&dir.join("SDKSettings.json"), "{\"Version\":\"11.0\",\"CanonicalName\":\"macosx11.0\"}\n")
}

fn write_file(path: &Path, text: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))
}

#[repr(C)]
struct DlInfo {
    fname: *const c_char,
    fbase: *mut c_void,
    sname: *const c_char,
    saddr: *mut c_void,
}

unsafe extern "C" {
    fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dladdr(addr: *const c_void, info: *mut DlInfo) -> c_int;
}

const RTLD_NOW: c_int = 0x2;
const RTLD_DEFAULT: *mut c_void = -2isize as *mut c_void;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_where_symbols_live() {
        assert_eq!(defining_image("_malloc").map(|i| library_for(&i, &BTreeSet::new())), Ok(LIBSYSTEM.to_owned()));
        assert!(defining_image("_faust_no_such_symbol").is_err());
        assert_eq!(library_for("/usr/lib/objc/libobjcMsgSend.dylib", &BTreeSet::new()), LIBOBJC);
        let foundation = framework_install_name("Foundation");
        assert!(foundation.starts_with("/System/Library/Frameworks/Foundation.framework/Versions/"), "{foundation}");
        let linked = BTreeSet::from([foundation.clone()]);
        let cf = "/System/Library/Frameworks/CoreFoundation.framework/Versions/A/CoreFoundation";
        assert_eq!(library_for(cf, &linked), foundation);
    }

    #[test]
    fn renders_stubs() {
        let mut e = Exports::default();
        for s in ["_malloc", "_OBJC_CLASS_$_NSObject", "_OBJC_METACLASS_$_NSObject", "_OBJC_EHTYPE_$_NSException"] {
            e.add(s);
        }
        let tbd = render_tbd(LIBSYSTEM, &e);
        assert!(tbd.contains("    symbols:         [ '_malloc' ]\n"), "{tbd}");
        assert!(tbd.contains("    objc-classes:    [ 'NSObject' ]\n"), "{tbd}");
        assert!(tbd.contains("    objc-eh-types:   [ 'NSException' ]\n"), "{tbd}");
        assert_eq!(
            render_tbd(LIBOBJC, &Exports::default()),
            format!(
                "--- !tapi-tbd\ntbd-version:     4\ntargets:         [ arm64-macos ]\ninstall-name:    '{LIBOBJC}'\n...\n"
            )
        );
    }
}
