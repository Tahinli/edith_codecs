//! Content-addressed cache of external-tool results, for test builds only.
//!
//! The gates shell out to ffmpeg (lavfi source generation, y4m and raw
//! loaders), aomenc (encodes) and aomdec / ffmpeg (oracle decodes). Those
//! results are deterministic functions of the program, its arguments, its
//! explicit environment, its working directory, its stdin and the contents of
//! every input file, so a second run can reuse them. The decoder under test is
//! never routed through here: only the external tool's `Output` is cached.
//!
//! Shell scripts are never cached: they can read hidden inputs (the control
//! tests' ffmpeg/aomdec shims read a sibling file), which no key can cover.
//!
//! A call is cached only when every path-like argument already names an
//! existing file (its content is hashed into the key) or a `/dev/` node. Any
//! other path-like argument is an output the tool would write, which cannot be
//! snapshotted, so that call simply runs. Only successful runs are stored.
//!
//! Environment: `EC_TOOL_CACHE=0|off` disables the cache; `EC_TOOL_CACHE_DIR`
//! overrides the directory (default `$HOME/.cache/ec-tool-cache`).

use std::ffi::OsStr;
use std::fs;
use std::io::{self, Read};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Output};
use std::time::{SystemTime, UNIX_EPOCH};

/// Outputs larger than this are run every time instead of stored.
const CAP_BYTES: u64 = 256 << 20;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
const BASIS_A: u64 = 0xcbf2_9ce4_8422_2325;
const BASIS_B: u64 = 0x8422_2325_cbf2_9ce4;

/// Two independent 64-bit FNV-1a passes, giving a 128-bit key without deps.
struct Hash {
    a: u64,
    b: u64,
}

impl Hash {
    fn new() -> Self {
        Hash {
            a: BASIS_A,
            b: BASIS_B,
        }
    }

    fn bytes(&mut self, data: &[u8]) {
        for &x in data {
            self.a = (self.a ^ u64::from(x)).wrapping_mul(FNV_PRIME);
            self.b = (self.b ^ u64::from(x)).wrapping_mul(FNV_PRIME);
        }
    }

    /// A length-prefixed field, so adjacent fields cannot run together.
    fn part(&mut self, data: &[u8]) {
        self.bytes(&u64::try_from(data.len()).unwrap_or(u64::MAX).to_le_bytes());
        self.bytes(data);
    }

    fn hex(&self) -> String {
        format!("{:016x}{:016x}", self.a, self.b)
    }
}

/// The cache directory, or `None` when the cache is disabled.
fn env_dir() -> Option<PathBuf> {
    if matches!(
        std::env::var("EC_TOOL_CACHE").as_deref(),
        Ok("0" | "off")
    ) {
        return None;
    }
    if let Some(d) = std::env::var_os("EC_TOOL_CACHE_DIR") {
        return Some(PathBuf::from(d));
    }
    Some(PathBuf::from(std::env::var_os("HOME")?).join(".cache/ec-tool-cache"))
}

/// Runs `cmd` through the cache (or straight through when disabled). `run`
/// performs the real spawn; it is called at most once.
///
/// # Panics
/// Never from the cache itself; `run` is infallible here, so the internal
/// `io::Result` is always `Ok`.
pub(crate) fn cached_output(
    cmd: &mut Command,
    stdin: Option<&[u8]>,
    run: impl FnOnce(&mut Command) -> Output,
) -> Output {
    match cached_output_io(cmd, stdin, |c| Ok(run(c))) {
        Ok(out) => out,
        Err(e) => panic!("tool cache: infallible run failed: {e}"),
    }
}

/// [`cached_output`] for callers that handle the `io::Result` of `.output()`.
pub(crate) fn cached_output_io(
    cmd: &mut Command,
    stdin: Option<&[u8]>,
    run: impl FnOnce(&mut Command) -> io::Result<Output>,
) -> io::Result<Output> {
    match env_dir() {
        None => run(cmd),
        Some(dir) => cached_output_in(&dir, cmd, stdin, run),
    }
}

fn cached_output_in(
    dir: &Path,
    cmd: &mut Command,
    stdin: Option<&[u8]>,
    run: impl FnOnce(&mut Command) -> io::Result<Output>,
) -> io::Result<Output> {
    let Some(key) = build_key(cmd, stdin) else {
        return run(cmd);
    };
    if let Some(hit) = load(dir, &key) {
        return Ok(hit);
    }
    let out = run(cmd)?;
    let size = u64::try_from(out.stdout.len()).unwrap_or(u64::MAX)
        + u64::try_from(out.stderr.len()).unwrap_or(u64::MAX);
    if out.status.success() && size <= CAP_BYTES {
        // A failed store only costs the next run its cache hit.
        let _ = store(dir, &key, &out);
    }
    Ok(out)
}

/// Resolves `prog` to a canonical path, searching PATH for bare names.
fn resolve_program(prog: &OsStr) -> Option<PathBuf> {
    let p = Path::new(prog);
    let found = if p.components().count() > 1 {
        p.to_path_buf()
    } else {
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path)
            .map(|d| d.join(prog))
            .find(|c| c.is_file())?
    };
    fs::canonicalize(found).ok()
}

/// A path-like argument: contains '/' and is not a `key:value` filter or a
/// string with spaces. Those are the arguments that could name an output.
fn path_like(c: &[u8]) -> bool {
    c.contains(&b'/') && !c.contains(&b':') && !c.contains(&b' ')
}

/// The argument itself and, for `--opt=path` / `key=path`, the part after the
/// first '='.
fn candidates(arg: &[u8]) -> [&[u8]; 2] {
    let after = arg
        .iter()
        .position(|&b| b == b'=')
        .map_or(&arg[0..0], |i| &arg[i + 1..]);
    [arg, after]
}

fn hash_file(h: &mut Hash, path: &Path) -> io::Result<()> {
    let mut f = fs::File::open(path)?;
    let len = f.metadata()?.len();
    h.bytes(b"file");
    h.bytes(&len.to_le_bytes());
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.bytes(&buf[..n]);
    }
    Ok(())
}

/// The key for this call, or `None` when the call must not be cached.
fn build_key(cmd: &Command, stdin: Option<&[u8]>) -> Option<String> {
    let prog = resolve_program(cmd.get_program())?;
    let meta = fs::metadata(&prog).ok()?;
    // A script can read state the key cannot see (the test shims read a
    // sibling `offsets` file), so only real binaries are cached.
    let mut magic = [0u8; 2];
    if fs::File::open(&prog).ok()?.read(&mut magic).ok()? == 2 && &magic == b"#!" {
        return None;
    }
    let mtime = meta
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_nanos();

    let mut h = Hash::new();
    h.part(b"ec-tool-cache/v1");
    h.part(prog.as_os_str().as_bytes());
    h.bytes(&meta.len().to_le_bytes());
    h.bytes(&mtime.to_le_bytes());

    let args: Vec<&OsStr> = cmd.get_args().collect();
    h.part(&u64::try_from(args.len()).ok()?.to_le_bytes());
    for arg in &args {
        h.part(arg.as_bytes());
    }

    // Relative paths resolve against the process cwd unless the command sets
    // its own, so both are part of the identity.
    h.part(std::env::current_dir().ok()?.as_os_str().as_bytes());
    match cmd.get_current_dir() {
        Some(d) => {
            h.bytes(b"\x01");
            h.part(d.as_os_str().as_bytes());
        }
        None => h.bytes(b"\x00"),
    }
    for (k, v) in cmd.get_envs() {
        h.part(k.as_bytes());
        match v {
            Some(v) => {
                h.bytes(b"\x01");
                h.part(v.as_bytes());
            }
            None => h.bytes(b"\x00"),
        }
    }

    match stdin {
        Some(data) => {
            h.bytes(b"stdin\x01");
            h.part(data);
        }
        None => h.bytes(b"stdin\x00"),
    }

    for arg in &args {
        for cand in candidates(arg.as_bytes()) {
            if cand.is_empty() {
                continue;
            }
            let os = OsStr::from_bytes(cand);
            let path = Path::new(os);
            if path.is_file() {
                hash_file(&mut h, path).ok()?;
            } else if path_like(cand) && !path.exists() && !cand.starts_with(b"/dev/") {
                return None;
            }
        }
    }
    Some(h.hex())
}

fn load(dir: &Path, key: &str) -> Option<Output> {
    let code = fs::read_to_string(dir.join(format!("{key}.code"))).ok()?;
    if code.trim() != "0" {
        return None;
    }
    Some(Output {
        status: ExitStatus::from_raw(0),
        stdout: fs::read(dir.join(format!("{key}.out"))).ok()?,
        stderr: fs::read(dir.join(format!("{key}.err"))).ok()?,
    })
}

fn store(dir: &Path, key: &str, out: &Output) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let nonce = format!("tmp.{}.{nanos}", std::process::id());
    let files: [(&str, &[u8]); 3] = [
        ("out", &out.stdout[..]),
        ("err", &out.stderr[..]),
        ("code", &b"0"[..]),
    ];
    for (ext, bytes) in files {
        let tmp = dir.join(format!("{key}.{nonce}.{ext}"));
        fs::write(&tmp, bytes)?;
        fs::rename(&tmp, dir.join(format!("{key}.{ext}")))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn second_call_replays_and_uncacheable_calls_always_run() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "ec-tool-cache-test-{}-{nanos}",
            std::process::id()
        ));
        let sh = || {
            let mut c = Command::new("sh");
            c.args(["-c", "echo $RANDOM$$"]);
            c
        };
        let first = cached_output_in(&dir, &mut sh(), None, |c| c.output()).unwrap();
        let second = cached_output_in(&dir, &mut sh(), None, |c| c.output()).unwrap();
        assert_eq!(first.stdout, second.stdout);

        // An output path that does not exist yet cannot be snapshotted, so
        // both calls must reach the run closure.
        let runs = Cell::new(0);
        let missing = dir.join("no-such-dir/out.bin");
        for _ in 0..2 {
            let mut c = Command::new("sh");
            c.args(["-c", "true"]).arg(&missing);
            cached_output_in(&dir, &mut c, None, |c| {
                runs.set(runs.get() + 1);
                c.output()
            })
            .unwrap();
        }
        assert_eq!(runs.get(), 2);
        let _ = fs::remove_dir_all(&dir);
    }
}
