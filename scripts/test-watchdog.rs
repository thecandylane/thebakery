//! The test watchdog: cargo's target runner under `scripts/test.sh`. Cargo
//! hands it each test binary; it runs the binary, passes its output on, and
//! fails a test still running at its time limit, so a stalled test fails
//! fast and by name instead of holding the run for hours:
//!
//!   TIMEOUT <test> after <n> s (limit <l> s)
//!
//! The binary is stopped there, with what its tests started; its tests that
//! hadn't finished run again without the stalled one, so the rest still
//! report; and the run fails. A test that had failed before the stop runs
//! again too, for its output: if it passes then, it's named FLAKY, and the
//! run still fails.
//!
//! The clock: libtest names a test that has run 60 seconds, so a limit
//! counts from the test's own start but can't end before that line; with one
//! test thread, libtest names each test as it starts. A binary that isn't
//! libtest's (a doctest) has the default limit for its whole run. A listing,
//! or a format for machines, passes through unwatched.
//!
//! A room may signal what runs in its own sandbox, so a stalled binary is
//! stopped there as anywhere. Where a sandbox refuses the signal, it can't
//! be: it's left to end on its own (its output is closed, so the next result
//! it writes ends it), it's said so, and the rest still run.
//!
//! Each test's time is worked out from the order libtest runs tests in (it
//! starts the next one as one ends) and added to the times file, for the
//! slowest tests `scripts/test.sh` lists.
//!
//!   test-watchdog --limits <file> [--times <file>] <test binary> [args...]
//!
//! Std only, so `rustc` alone builds it (`scripts/test.sh` does); its own
//! tests are below.

use std::collections::{HashMap, HashSet};
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

/// libtest names a test still running after this long.
const NAMED_AFTER: Duration = Duration::from_secs(60);
/// How long output is still read once a binary has ended or been stopped:
/// a process it started may hold its output open.
const DRAIN: Duration = Duration::from_secs(2);
/// How long a binary may go on running after its summary line.
const AFTER_SUMMARY: Duration = Duration::from_secs(60);
/// How far a test's worked-out start may be from the one libtest's own
/// 60-second line implies before the times are dropped as unreliable.
const SLACK: Duration = Duration::from_secs(5);
const TICK: Duration = Duration::from_millis(100);
/// libtest's exit code for failed tests; a timeout's too.
const FAILED: i32 = 101;

mod sys {
    extern "C" {
        pub fn kill(pid: i32, sig: i32) -> i32;
        pub fn getppid() -> i32;
        pub fn signal(sig: i32, handler: usize) -> usize;
        pub fn _exit(code: i32) -> !;
    }
    pub const SIGHUP: i32 = 1;
    pub const SIGINT: i32 = 2;
    pub const SIGKILL: i32 = 9;
    pub const SIGTERM: i32 = 15;
}

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut limits, mut times) = (None, None);
    let binary = loop {
        match args.next().as_deref() {
            Some("--limits") => limits = args.next(),
            Some("--times") => times = args.next().map(PathBuf::from),
            Some(binary) if !binary.starts_with("--") => break PathBuf::from(binary),
            _ => usage(),
        }
    };
    let Some(limits) = limits else { usage() };
    let limits = std::fs::read_to_string(&limits)
        .map_err(|e| e.to_string())
        .and_then(|text| Limits::parse(&text))
        .unwrap_or_else(|e| {
            eprintln!("test-watchdog: {limits}: {e}");
            std::process::exit(2)
        });
    let args: Vec<String> = args.collect();
    let signals = may_signal();
    if signals {
        forward_signals();
    }
    let mut watch = Watch {
        binary,
        package: std::env::var("CARGO_PKG_NAME").unwrap_or_else(|_| "?".into()),
        limits,
        times,
        signals,
        out: Box::new(io::stdout()),
        stop: Box::new(stop_by_signal),
        threads: available_threads(),
        at_line_start: true,
    };
    std::process::exit(watch.run(&args));
}

fn usage() -> ! {
    eprintln!("usage: test-watchdog --limits <file> [--times <file>] <test binary> [args...]");
    std::process::exit(2)
}

/// Whether this process may signal others: not in a sandbox that refuses a
/// signal (here asked of the parent, as signal 0 only checks; a room allows
/// one within its own sandbox).
fn may_signal() -> bool {
    unsafe { sys::kill(sys::getppid(), 0) == 0 }
}

/// The running binary's process group, for the signal handler.
static GROUP: AtomicI32 = AtomicI32::new(0);

/// Where it may signal, the binary runs in a process group of its own, so a
/// stop takes what its tests started too; so that group is stopped here
/// when this is interrupted or ended, as the terminal would have.
fn forward_signals() {
    extern "C" fn on_signal(sig: i32) {
        let group = GROUP.load(Ordering::SeqCst);
        unsafe {
            if group > 0 {
                sys::kill(-group, sys::SIGKILL);
            }
            sys::_exit(128 + sig)
        }
    }
    for sig in [sys::SIGHUP, sys::SIGINT, sys::SIGTERM] {
        unsafe { sys::signal(sig, on_signal as extern "C" fn(i32) as usize) };
    }
}

/// Stops a binary, with its process group when it has one of its own; false
/// when the signal is refused.
fn stop_by_signal(child: &mut Child, group: bool) -> bool {
    let pid = child.id() as i32;
    let stopped = unsafe {
        (group && sys::kill(-pid, sys::SIGKILL) == 0) || sys::kill(pid, sys::SIGKILL) == 0
    };
    if stopped {
        let _ = child.wait();
    }
    stopped
}

/// libtest's own default: as many test threads as the machine runs at once.
fn available_threads() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get())
}

// ─── Limits ─────────────────────────────────────────────────────────────────

/// Time limits: a default, and known-slow tests' own, by package and name.
struct Limits {
    default: Duration,
    tests: HashMap<(String, String), Duration>,
}

impl Limits {
    /// `default <seconds>` once, and `<seconds> <package> <test>` lines;
    /// `#` starts a comment.
    fn parse(text: &str) -> Result<Limits, String> {
        let (mut default, mut tests) = (None, HashMap::new());
        for (n, line) in text.lines().enumerate() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let bad = || {
                format!("line {}: {line:?}: want `default <seconds>` or `<seconds> <package> <test>`", n + 1)
            };
            let secs = |s: &str| s.parse::<u64>().ok().filter(|&s| s > 0).map(Duration::from_secs);
            match line.split_whitespace().collect::<Vec<_>>()[..] {
                ["default", s] if default.is_none() => default = Some(secs(s).ok_or_else(bad)?),
                [s, package, test] => {
                    let limit = secs(s).ok_or_else(bad)?;
                    if tests.insert((package.to_string(), test.to_string()), limit).is_some() {
                        return Err(format!("{}: listed twice", bad()));
                    }
                }
                _ => return Err(bad()),
            }
        }
        let default = default.ok_or("no `default <seconds>` line")?;
        Ok(Limits { default, tests })
    }

    fn of(&self, package: &str, test: &str) -> Duration {
        let key = (package.to_string(), test.to_string());
        self.tests.get(&key).copied().unwrap_or(self.default)
    }
}

// ─── libtest's output and arguments ─────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq)]
enum Outcome {
    Passed,
    Failed,
    Ignored,
}

#[derive(Debug, PartialEq)]
enum Line<'a> {
    /// `running 3 tests`: libtest's run begins.
    Running(usize),
    /// `test a::b ... ok`
    Done(&'a str, Outcome),
    /// `test a::b ... ` and then something else: what a test's own child
    /// printed came between the name and its result.
    Begun(&'a str),
    /// `test a::b has been running for over 60 seconds`
    Named(&'a str),
    /// `test result: ...`
    Summary,
    /// A bare `ok`, `FAILED` or `ignored`: the result of the test just begun.
    Result(Outcome),
    Other,
}

fn outcome(word: &str) -> Option<Outcome> {
    match word {
        "ok" => Some(Outcome::Passed),
        "FAILED" => Some(Outcome::Failed),
        w if w == "ignored" || w.starts_with("ignored, ") => Some(Outcome::Ignored),
        _ => None,
    }
}

fn parse_line(line: &str) -> Line<'_> {
    if let Some(n) = line.strip_prefix("running ") {
        let n = n.strip_suffix(" tests").or_else(|| n.strip_suffix(" test"));
        if let Some(n) = n.and_then(|n| n.parse().ok()) {
            return Line::Running(n);
        }
    }
    if line.starts_with("test result: ") {
        return Line::Summary;
    }
    if let Some(rest) = line.strip_prefix("test ") {
        if let Some(name) = rest.strip_suffix(" has been running for over 60 seconds") {
            return Line::Named(name);
        }
        if let Some((name, result)) = rest.split_once(" ... ") {
            let name = name.strip_suffix(" - should panic").unwrap_or(name);
            return match outcome(result) {
                Some(outcome) => Line::Done(name, outcome),
                None => Line::Begun(name),
            };
        }
    }
    // What a test's child printed without ending its line, then libtest's.
    if let Some(at) = line.rfind("test ").filter(|&at| at > 0) {
        if let found @ (Line::Done(..) | Line::Named(_)) = parse_line(&line[at..]) {
            return found;
        }
    }
    outcome(line.trim()).map_or(Line::Other, Line::Result)
}

/// The test named in a line libtest has begun and not ended: with one test
/// thread, `test a::b ... ` is written as the test starts.
fn started(partial: &str) -> Option<&str> {
    let name = partial.strip_prefix("test ")?.strip_suffix(" ... ")?;
    Some(name.strip_suffix(" - should panic").unwrap_or(name))
}

/// libtest's options that take a value as the next argument.
const VALUED: [&str; 7] = ["--test-threads", "--logfile", "--skip", "--color", "--format", "-Z", "--shuffle-seed"];

/// A binary's arguments as libtest reads them: options (with a value when
/// they take one) and name filters.
#[derive(Debug, PartialEq)]
enum Arg<'a> {
    Option(&'a str, Option<&'a str>),
    Filter(&'a str),
}

impl Arg<'_> {
    fn name(&self) -> &str {
        match self {
            Arg::Option(o, _) => o.split_once('=').map_or(*o, |(name, _)| name),
            Arg::Filter(f) => f,
        }
    }

    fn value(&self) -> Option<&str> {
        match self {
            Arg::Option(o, value) => value.or_else(|| o.split_once('=').map(|(_, v)| v)),
            Arg::Filter(_) => None,
        }
    }

    fn push_to(&self, out: &mut Vec<String>) {
        match self {
            Arg::Option(o, value) => out.extend(std::iter::once(*o).chain(*value).map(String::from)),
            Arg::Filter(f) => out.push(f.to_string()),
        }
    }
}

fn parse_args(args: &[String]) -> Vec<Arg<'_>> {
    let mut out = Vec::new();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        if !arg.starts_with('-') {
            out.push(Arg::Filter(arg));
        } else if VALUED.contains(&arg.as_str()) {
            out.push(Arg::Option(arg, it.next().map(String::as_str)));
        } else {
            out.push(Arg::Option(arg, None));
        }
    }
    out
}

/// Whether these arguments run tests to be watched one by one: not a
/// listing, its help, benchmarks, or a format for machines.
fn watchable(args: &[Arg]) -> bool {
    !args.iter().any(|a| {
        matches!(a.name(), "--list" | "--help" | "-h" | "--bench")
            || (a.name() == "--format" && matches!(a.value(), Some("json" | "junit")))
    })
}

/// The test threads libtest will use with these arguments.
fn threads(args: &[Arg], available: usize) -> usize {
    let asked = args.iter().rev().find(|a| a.name() == "--test-threads").and_then(|a| a.value());
    let env = std::env::var("RUST_TEST_THREADS").ok();
    asked.or(env.as_deref()).and_then(|n| n.parse().ok()).filter(|&n| n > 0).unwrap_or(available)
}

/// Whether libtest runs tests in its listed order: not when shuffling.
fn in_order(args: &[Arg]) -> bool {
    let env = |name| std::env::var_os(name).is_some();
    !args.iter().any(|a| matches!(a.name(), "--shuffle" | "--shuffle-seed"))
        && !env("RUST_TEST_SHUFFLE")
        && !env("RUST_TEST_SHUFFLE_SEED")
}

/// The arguments that list what these would run.
fn list_args(args: &[Arg]) -> Vec<String> {
    let mut out = Vec::new();
    for arg in args {
        if !matches!(arg.name(), "--logfile" | "--format" | "-q" | "--quiet") {
            arg.push_to(&mut out);
        }
    }
    out.push("--list".into());
    out
}

/// The arguments that run exactly `tests`, with the original's options.
fn rerun_args(args: &[Arg], tests: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for arg in args {
        if !matches!(arg, Arg::Filter(_)) && !matches!(arg.name(), "--skip" | "--exact") {
            arg.push_to(&mut out);
        }
    }
    out.push("--exact".into());
    out.extend(tests.iter().cloned());
    out
}

// ─── Times ──────────────────────────────────────────────────────────────────

/// What a run reported about one test, and when.
#[derive(Debug)]
enum Event {
    Ended(String),
    Named(String),
}

/// Each test's time, worked out from libtest's order: it starts `threads`
/// tests in the order it lists them, then the next as each one ends. None
/// when what was seen doesn't fit that, rather than a wrong time.
fn times(todo: &[String], threads: usize, began: Instant, events: &[(Instant, Event)]) -> Option<Vec<(String, Duration)>> {
    let mut queue = todo.iter();
    let mut running: HashMap<&str, Instant> = HashMap::new();
    for name in queue.by_ref().take(threads.max(1)) {
        running.insert(name, began);
    }
    let mut out = Vec::new();
    for (at, event) in events {
        match event {
            Event::Ended(name) => {
                let since = running.remove(name.as_str())?;
                out.push((name.clone(), at.saturating_duration_since(since)));
                if let Some(next) = queue.next() {
                    running.insert(next, *at);
                }
            }
            Event::Named(name) => {
                let ran = at.saturating_duration_since(*running.get(name.as_str())?);
                if ran + SLACK < NAMED_AFTER || ran > NAMED_AFTER + SLACK {
                    return None;
                }
            }
        }
    }
    Some(out)
}

// ─── Running a binary ───────────────────────────────────────────────────────

/// One run of a binary: what it reported, and how it ended.
#[derive(Default)]
struct Run {
    /// When libtest's `running N tests` came, and N.
    began: Option<(Instant, usize)>,
    passed: Vec<String>,
    failed: Vec<String>,
    ignored: Vec<String>,
    events: Vec<(Instant, Event)>,
    /// The test that outran its limit: its name, how long it ran, its limit.
    stalled: Option<(String, Duration, Duration)>,
    /// Why the whole binary was stopped, when it was.
    binary_stalled: Option<String>,
    /// Whether a stop took.
    stopped: bool,
    /// The binary's process, when it had to be left running.
    left: Option<u32>,
    /// When libtest's summary line came.
    summary: Option<Instant>,
    status: Option<ExitStatus>,
}

impl Run {
    fn count(&mut self, name: &str, outcome: Outcome) {
        match outcome {
            Outcome::Passed => self.passed.push(name.to_string()),
            Outcome::Failed => self.failed.push(name.to_string()),
            Outcome::Ignored => self.ignored.push(name.to_string()),
        }
    }
}

/// The tests of one run believed to be running, and since when: those
/// libtest named at 60 seconds, and with one thread each as it starts.
#[derive(Default)]
struct Clocks {
    running: HashMap<String, Instant>,
    /// With one thread: the test libtest began last.
    current: Option<String>,
    /// A test whose result line was split: its result is the next bare one.
    unresolved: Option<String>,
}

impl Clocks {
    fn end(&mut self, name: &str) {
        self.running.remove(name);
        if self.current.as_deref() == Some(name) {
            self.current = None;
        }
    }

    /// One thread: a test begins, so the one before it has ended.
    fn begin(&mut self, name: &str, at: Instant) -> Option<String> {
        if self.current.as_deref() == Some(name) {
            return None;
        }
        let before = self.current.replace(name.to_string());
        if let Some(before) = &before {
            self.running.remove(before);
        }
        self.running.insert(name.to_string(), at);
        before
    }
}

struct Watch {
    binary: PathBuf,
    package: String,
    limits: Limits,
    times: Option<PathBuf>,
    /// Whether this may signal its child ([`may_signal`]).
    signals: bool,
    out: Box<dyn Write>,
    stop: Box<dyn Fn(&mut Child, bool) -> bool>,
    threads: usize,
    /// Whether the output so far ends a line.
    at_line_start: bool,
}

impl Watch {
    /// A line of the watchdog's own, on a line of its own.
    fn say(&mut self, text: &str) {
        if !self.at_line_start {
            let _ = self.out.write_all(b"\n");
        }
        self.at_line_start = text.ends_with('\n');
        let _ = self.out.write_all(text.as_bytes());
        let _ = self.out.flush();
    }

    /// Runs the binary with `args`, test by test when they're libtest's, and
    /// gives its exit code.
    fn run(&mut self, args: &[String]) -> i32 {
        let parsed = parse_args(args);
        if !watchable(&parsed) {
            let run = self.run_once(args, false, false);
            return self.code_of(&run);
        }
        let threads = threads(&parsed, self.threads);
        let timed = self.times.is_some() && in_order(&parsed);
        let mut run_args = args.to_vec();
        // The tests this run was asked for, once listed.
        let mut todo: Option<Vec<String>> = None;
        let mut failed_before_stop: Vec<String> = Vec::new();
        let mut passed_after_stop: HashSet<String> = HashSet::new();
        let mut failed = false;
        loop {
            let run = self.run_once(&run_args, threads == 1, true);
            if run.began.is_none() {
                // Not libtest's, or stopped before its tests began.
                return self.code_of(&run);
            }
            if todo.is_none() && (timed || run.stalled.is_some()) {
                todo = self.list(&parsed);
            }
            if timed {
                self.record_times(todo.as_deref(), threads, &run);
            }
            if !failed_before_stop.is_empty() {
                passed_after_stop.extend(run.passed.iter().cloned());
            }
            let Some((name, ran, limit)) = run.stalled.clone() else {
                failed |= run.binary_stalled.is_some() || !run.status.is_some_and(|s| s.success());
                if let Some(why) = &run.binary_stalled {
                    let msg = format!("TIMEOUT {} ({}): {why}\n", self.package, self.binary.display());
                    self.say(&msg);
                }
                break;
            };
            failed = true;
            failed_before_stop.extend(run.failed.iter().cloned());
            let timeout = format!("TIMEOUT {name} after {} s (limit {} s)", ran.as_secs(), limit.as_secs());
            let how = if run.stopped {
                "stopped its test binary".to_string()
            } else {
                format!(
                    "the sandbox refused the stop, so its test binary (pid {}) is left to end on its own",
                    run.left.unwrap_or(0)
                )
            };
            let finished: HashSet<&String> = run.passed.iter().chain(&run.ignored).chain([&name]).collect();
            let rest: Option<Vec<String>> =
                todo.as_ref().map(|all| all.iter().filter(|t| !finished.contains(t)).cloned().collect());
            let again = match rest.as_ref().map(Vec::len) {
                None => "its tests couldn't be listed, so the rest don't run again".to_string(),
                Some(0) => "none of its tests is left to run".to_string(),
                Some(1) => "its 1 unfinished test runs again".to_string(),
                Some(n) => format!("its {n} unfinished tests run again"),
            };
            // The timeout, and the failures that won't run again to count
            // themselves.
            let failures = 1 + if rest.is_some() { 0 } else { run.failed.len() };
            let took = run.began.map_or(ran, |(t, _)| t.elapsed());
            self.say(&format!(
                "{timeout}\n  {how}; {again}\ntest result: FAILED. {} passed; {failures} failed; {} ignored; 0 measured; 0 filtered out; finished in {:.2}s\n\n",
                run.passed.len(),
                run.ignored.len(),
                took.as_secs_f64(),
            ));
            match rest {
                Some(rest) if !rest.is_empty() => {
                    run_args = rerun_args(&parsed, &rest);
                    todo = Some(rest);
                }
                _ => break,
            }
        }
        for name in failed_before_stop.iter().filter(|t| passed_after_stop.contains(*t)) {
            self.say(&format!("FLAKY {name}: failed before its binary was stopped, then passed when run again\n"));
        }
        if failed {
            FAILED
        } else {
            0
        }
    }

    /// A binary not watched test by test: its own exit code, or a timeout.
    fn code_of(&mut self, run: &Run) -> i32 {
        if let Some(why) = &run.binary_stalled {
            let msg = format!("TIMEOUT {} ({}): {why}\n", self.package, self.binary.display());
            self.say(&msg);
            return FAILED;
        }
        match run.status {
            Some(status) => status.code().unwrap_or(FAILED),
            None => FAILED,
        }
    }

    /// The binary's tests these arguments select, in libtest's order.
    fn list(&mut self, args: &[Arg]) -> Option<Vec<String>> {
        let mut child = Command::new(&self.binary)
            .args(list_args(args))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .spawn()
            .ok()?;
        let rx = read_chunks(child.stdout.take()?);
        let deadline = Instant::now() + self.limits.default;
        let mut out = Vec::new();
        loop {
            match rx.recv_timeout(TICK) {
                Ok(chunk) => out.extend(chunk),
                Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) if Instant::now() < deadline => {}
                Err(RecvTimeoutError::Timeout) => {
                    (self.stop)(&mut child, false);
                    return None;
                }
            }
        }
        if !child.wait().ok()?.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&out);
        Some(text.lines().filter_map(|l| l.strip_suffix(": test")).map(String::from).collect())
    }

    fn record_times(&mut self, todo: Option<&[String]>, threads: usize, run: &Run) {
        let (Some(path), Some(todo), Some((began, planned))) = (&self.times, todo, run.began) else {
            return;
        };
        let mut rows = String::new();
        if planned == todo.len() {
            for (name, took) in times(todo, threads, began, &run.events).unwrap_or_default() {
                rows.push_str(&format!("{:.1}\t{}\t{name}\n", took.as_secs_f64(), self.package));
            }
        }
        if let Some((name, ran, _)) = &run.stalled {
            rows.push_str(&format!("{:.1}\t{}\t{name}\tTIMEOUT\n", ran.as_secs_f64(), self.package));
        }
        let file = std::fs::OpenOptions::new().create(true).append(true).open(path);
        if let Ok(mut file) = file {
            let _ = file.write_all(rows.as_bytes());
        }
    }

    /// Runs the binary once, passing its output on and, when `watched`,
    /// keeping its tests' clocks. `serial`: libtest runs one test at a time,
    /// naming each as it starts.
    fn run_once(&mut self, args: &[String], serial: bool, watched: bool) -> Run {
        use std::os::unix::process::CommandExt;
        let mut cmd = Command::new(&self.binary);
        // Both outputs through here: a binary left running (its stop refused) then
        // holds only this process's pipes, never the run's own output.
        cmd.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        if self.signals {
            cmd.process_group(0);
        }
        let mut run = Run::default();
        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(e) => {
                let msg = format!("test-watchdog: {}: {e}\n", self.binary.display());
                self.say(&msg);
                return run;
            }
        };
        if self.signals {
            GROUP.store(child.id() as i32, Ordering::SeqCst);
        }
        let mut stderr = child.stderr.take().expect("piped");
        std::thread::spawn(move || io::copy(&mut stderr, &mut io::stderr()));
        let rx = read_chunks(child.stdout.take().expect("piped"));
        let spawned = Instant::now();
        let mut pending: Vec<u8> = Vec::new();
        let mut clocks = Clocks::default();
        let mut ended: Option<Instant> = None;
        let mut closed = false;
        loop {
            // Its output closed, its clocks still run until it has ended.
            let chunk = if closed {
                std::thread::sleep(TICK);
                Err(RecvTimeoutError::Disconnected)
            } else {
                rx.recv_timeout(TICK)
            };
            let now = Instant::now();
            match chunk {
                Ok(chunk) => {
                    self.forward(&chunk);
                    pending.extend(chunk);
                    while let Some(at) = pending.iter().position(|&b| b == b'\n') {
                        let line: Vec<u8> = pending.drain(..=at).collect();
                        let line = String::from_utf8_lossy(&line);
                        run.read(line.trim_end_matches(|c| c == '\n' || c == '\r'), now, serial, &mut clocks, spawned);
                    }
                    // A name and its result are short: past that, it isn't one.
                    if serial && pending.len() < 4096 {
                        if let Some(name) = started(&String::from_utf8_lossy(&pending)) {
                            if let Some(before) = clocks.begin(name, now) {
                                run.events.push((now, Event::Ended(before)));
                            }
                        }
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => closed = true,
            }
            if run.status.is_none() {
                if let Ok(Some(status)) = child.try_wait() {
                    run.status = Some(status);
                    ended = Some(now);
                }
            }
            if run.status.is_some() && (closed || ended.is_some_and(|t| now.duration_since(t) >= DRAIN)) {
                break;
            }
            if run.status.is_some() || !watched {
                continue;
            }
            let why = if run.began.is_none() && now.duration_since(spawned) >= self.limits.default {
                Some(format!("still running after {} s, its limit", self.limits.default.as_secs()))
            } else {
                run.summary
                    .filter(|t| now.duration_since(*t) >= AFTER_SUMMARY)
                    .map(|_| format!("still running {} s after its tests ended", AFTER_SUMMARY.as_secs()))
            };
            let overdue = clocks
                .running
                .iter()
                .map(|(name, since)| (name, now.duration_since(*since), self.limits.of(&self.package, name)))
                .filter(|(_, ran, limit)| ran >= limit)
                .max_by_key(|(_, ran, _)| *ran)
                .map(|(name, ran, limit)| (name.clone(), ran, limit));
            if why.is_none() && overdue.is_none() {
                continue;
            }
            run.binary_stalled = why;
            run.stalled = overdue;
            run.stopped = (self.stop)(&mut child, self.signals);
            if run.stopped {
                run.status = child.try_wait().ok().flatten();
                // What's still in the pipe, briefly: a process the binary
                // started may hold it open.
                let deadline = Instant::now() + DRAIN;
                while let Ok(chunk) = rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                    self.forward(&chunk);
                }
            } else {
                run.left = Some(child.id());
            }
            break;
        }
        GROUP.store(0, Ordering::SeqCst);
        if self.signals && run.status.is_some() {
            // Whatever its tests left running in its group.
            unsafe { sys::kill(-(child.id() as i32), sys::SIGKILL) };
        }
        run
    }

    /// The binary's own output, passed on as it comes.
    fn forward(&mut self, bytes: &[u8]) {
        if let Some(&last) = bytes.last() {
            self.at_line_start = last == b'\n';
        }
        let _ = self.out.write_all(bytes);
        let _ = self.out.flush();
    }
}

impl Run {
    /// One line of the binary's output.
    fn read(&mut self, line: &str, now: Instant, serial: bool, clocks: &mut Clocks, spawned: Instant) {
        match parse_line(line) {
            Line::Running(n) => self.began = Some((now, n)),
            Line::Done(name, outcome) => {
                clocks.end(name);
                self.count(name, outcome);
                self.events.push((now, Event::Ended(name.to_string())));
            }
            Line::Begun(name) if serial => {
                if let Some(before) = clocks.begin(name, now) {
                    self.events.push((now, Event::Ended(before)));
                }
            }
            // Several threads: libtest writes a result right after its
            // name, so a split one has ended; its result is the next bare one.
            Line::Begun(name) => {
                clocks.end(name);
                clocks.unresolved = Some(name.to_string());
                self.events.push((now, Event::Ended(name.to_string())));
            }
            Line::Result(outcome) => {
                let name = if serial { clocks.current.take() } else { clocks.unresolved.take() };
                if let Some(name) = name {
                    clocks.end(&name);
                    self.count(&name, outcome);
                    if serial {
                        self.events.push((now, Event::Ended(name)));
                    }
                }
            }
            Line::Named(name) => {
                let since = now.checked_sub(NAMED_AFTER).unwrap_or(spawned);
                clocks.running.entry(name.to_string()).or_insert(since);
                self.events.push((now, Event::Named(name.to_string())));
            }
            Line::Summary => {
                self.summary = Some(now);
                clocks.running.clear();
                clocks.current = None;
            }
            Line::Other => {}
        }
    }
}

/// A pipe's bytes as they come, from a thread of its own; the channel closes
/// at the end of the pipe. A reader left blocked on a pipe something else
/// still holds ends with this process.
fn read_chunks(mut pipe: impl Read + Send + 'static) -> Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match pipe.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    // ─── Pieces ─────────────────────────────────────────────────────────────

    /// The lines are libtest's own, as Rust 1.98 writes them.
    #[test]
    fn reads_libtest_lines() {
        assert_eq!(parse_line("running 7 tests"), Line::Running(7));
        assert_eq!(parse_line("running 1 test"), Line::Running(1));
        assert_eq!(parse_line("test tests::a_fast ... ok"), Line::Done("tests::a_fast", Outcome::Passed));
        assert_eq!(parse_line("test tests::b_fails ... FAILED"), Line::Done("tests::b_fails", Outcome::Failed));
        assert_eq!(parse_line("test tests::d_bare ... ignored"), Line::Done("tests::d_bare", Outcome::Ignored));
        assert_eq!(
            parse_line("test tests::c_ignored_with_reason ... ignored, a reason"),
            Line::Done("tests::c_ignored_with_reason", Outcome::Ignored)
        );
        assert_eq!(
            parse_line("test tests::e_should_panic - should panic ... ok"),
            Line::Done("tests::e_should_panic", Outcome::Passed)
        );
        assert_eq!(
            parse_line("test tests::f_slow has been running for over 60 seconds"),
            Line::Named("tests::f_slow")
        );
        assert_eq!(
            parse_line("test result: FAILED. 4 passed; 1 failed; 2 ignored; 0 measured; 0 filtered out; finished in 0.00s"),
            Line::Summary
        );
        assert_eq!(parse_line("ok"), Line::Result(Outcome::Passed));
        assert_eq!(parse_line("test tests::x ... what a child printed"), Line::Begun("tests::x"));
        assert_eq!(parse_line("---- tests::b_fails stdout ----"), Line::Other);
        assert_eq!(parse_line("a child's unended linetest a::b ... ok"), Line::Done("a::b", Outcome::Passed));
        assert_eq!(
            parse_line("a child's unended linetest a::b has been running for over 60 seconds"),
            Line::Named("a::b")
        );
        assert_eq!(parse_line("this test is fine ... no"), Line::Other, "not libtest's");
        assert_eq!(started("test tests::f_slow ... "), Some("tests::f_slow"));
        assert_eq!(started("test tests::e - should panic ... "), Some("tests::e"));
        assert_eq!(started("test tests::f_slow ... ok"), None);
    }

    #[test]
    fn limits_come_from_a_default_and_known_slow_tests() {
        let limits = Limits::parse("# comment\ndefault 120\n\n600 bakery-node a::slow  # why\n").unwrap();
        assert_eq!(limits.of("bakery-node", "a::slow"), Duration::from_secs(600));
        assert_eq!(limits.of("bakery-exec", "a::slow"), Duration::from_secs(120), "by package too");
        assert_eq!(limits.of("bakery-node", "a::other"), Duration::from_secs(120));
        for bad in [
            "600 bakery-node a::slow\n",
            "default 0\n",
            "default x\n",
            "default 1\ndefault 2\n",
            "default 1\n5 a\n",
            "default 1\n5 a b\n5 a b\n",
        ] {
            assert!(Limits::parse(bad).is_err(), "{bad:?}");
        }
    }

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    /// A rerun keeps the run's options and names its tests exactly; the
    /// listing keeps its filters.
    #[test]
    fn reruns_and_listings_keep_the_runs_options() {
        let args = strings(&[
            "routes::git::",
            "--test-threads",
            "4",
            "--skip",
            "slow",
            "--nocapture",
            "--include-ignored",
            "--color=never",
            "-q",
        ]);
        let parsed = parse_args(&args);
        assert_eq!(threads(&parsed, 10), 4);
        assert!(watchable(&parsed) && in_order(&parsed));
        assert_eq!(
            rerun_args(&parsed, &strings(&["a::x", "b::y"])),
            strings(&["--test-threads", "4", "--nocapture", "--include-ignored", "--color=never", "-q", "--exact", "a::x", "b::y"])
        );
        assert_eq!(
            list_args(&parsed),
            strings(&["routes::git::", "--test-threads", "4", "--skip", "slow", "--nocapture", "--include-ignored", "--color=never", "--list"])
        );
        assert_eq!(threads(&parse_args(&strings(&["--test-threads=1"])), 10), 1);
        assert!(!watchable(&parse_args(&strings(&["--list"]))));
        assert!(!watchable(&parse_args(&strings(&["-Z", "unstable-options", "--format", "json"]))));
        assert!(watchable(&parse_args(&strings(&["--format=pretty"]))));
        assert!(!in_order(&parse_args(&strings(&["--shuffle"]))));
    }

    /// Times follow libtest's order: `threads` start together, then the
    /// next starts as one ends; what doesn't fit gives no times at all.
    #[test]
    fn times_follow_the_order_libtest_runs_tests_in() {
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let todo = strings(&["a", "b", "c", "d"]);
        let ended = |s, n: &str| (at(s), Event::Ended(n.to_string()));
        // a and b start at 0; b ends at 2 and c starts; a ends at 5 and d
        // starts; d ends at 6; c is named at 62 and ends at 70.
        let events = vec![ended(2, "b"), ended(5, "a"), ended(6, "d"), (at(62), Event::Named("c".into())), ended(70, "c")];
        let got: HashMap<String, u64> =
            times(&todo, 2, t0, &events).unwrap().into_iter().map(|(n, d)| (n, d.as_secs())).collect();
        assert_eq!(got, HashMap::from([("a".into(), 5), ("b".into(), 2), ("c".into(), 68), ("d".into(), 1)]));
        assert!(times(&todo, 2, t0, &[ended(2, "c")]).is_none(), "c hadn't started");
        let named_early = vec![ended(2, "b"), (at(30), Event::Named("c".into()))];
        assert!(times(&todo, 2, t0, &named_early).is_none(), "c had run 28 s, not 60");
    }

    // ─── Whole runs, with stand-ins for test binaries ───────────────────────

    /// A folder of its own for one test, gone at the end.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let dir = std::env::temp_dir().join(format!("test-watchdog-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }

        /// A stand-in test binary: a shell script.
        fn script(&self, name: &str, body: &str) -> PathBuf {
            let path = self.0.join(name);
            std::fs::write(&path, format!("#!/bin/sh\ndir='{}'\n{body}", self.0.display())).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path
        }

        fn read(&self, name: &str) -> String {
            std::fs::read_to_string(self.0.join(name)).unwrap_or_default()
        }

        /// A stop that needs no signal, so it works where a signal is
        /// refused as anywhere: the stand-in ends itself once its folder
        /// holds `stop`.
        fn stop_by_file(&self) -> Box<dyn Fn(&mut Child, bool) -> bool> {
            let stop = self.0.join("stop");
            Box::new(move |child: &mut Child, _| {
                std::fs::write(&stop, b"").unwrap();
                let _ = child.wait();
                true
            })
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::write(self.0.join("stop"), b"");
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[derive(Clone, Default)]
    struct Output(Arc<Mutex<Vec<u8>>>);

    impl Write for Output {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Output {
        fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
        }
    }

    fn watch(binary: &Path, limits: &str, stop: Box<dyn Fn(&mut Child, bool) -> bool>, out: &Output) -> Watch {
        Watch {
            binary: binary.to_path_buf(),
            package: "pkg".into(),
            limits: Limits::parse(limits).unwrap(),
            times: None,
            signals: false,
            out: Box::new(out.clone()),
            stop,
            threads: 4,
            at_line_start: true,
        }
    }

    /// A libtest stand-in with tests a, b (fails unless `b-passes` is
    /// there), c (still running when the stall is caught) and stall (named
    /// at 60 seconds straight away, then never ending). It writes each
    /// run's arguments to `args`.
    const LIBTEST: &str = r#"
echo "$*" >> "$dir/args"
case " $* " in
*" --list "*)
  printf 'a: test\nb: test\nc: test\nstall: test\n\n4 tests, 0 benchmarks\n' ;;
*" --exact "*)
  while [ "$1" != "--exact" ]; do shift; done; shift
  printf '\nrunning %s tests\n' "$#"
  ok=0; failed=0
  for n in "$@"; do
    if [ "$n" = b ] && [ ! -e "$dir/b-passes" ]; then echo "test b ... FAILED"; failed=$((failed+1)); else echo "test $n ... ok"; ok=$((ok+1)); fi
  done
  if [ $failed -gt 0 ]; then printf '\nfailures:\n\n---- b stdout ----\nb went wrong\n'; fi
  result=ok; [ $failed -gt 0 ] && result=FAILED
  printf '\ntest result: %s. %s passed; %s failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n\n' $result $ok $failed
  [ $failed -eq 0 ] ;;
*)
  printf '\nrunning 4 tests\n'
  echo "test a ... ok"
  echo "test b ... FAILED"
  echo "test stall has been running for over 60 seconds"
  while [ -d "$dir" ] && [ ! -e "$dir/stop" ]; do sleep 0.05; done
  exit 137 ;;
esac
"#;

    /// The stalled test fails by name, its binary stopped; its tests that
    /// hadn't finished (and the one that had failed, for its output) run
    /// again, so the rest still report; the totals count each test once;
    /// the run fails.
    #[test]
    fn a_stalled_test_fails_by_name_and_the_rest_still_run() {
        let dir = Scratch::new("stall");
        let binary = dir.script("libtest", LIBTEST);
        let out = Output::default();
        let mut w = watch(&binary, "default 61\n", dir.stop_by_file(), &out);
        w.times = Some(dir.0.join("times"));
        let started = Instant::now();
        let code = w.run(&[]);
        let text = out.text();
        assert_eq!(code, FAILED, "{text}");
        assert!(started.elapsed() < Duration::from_secs(10), "caught at its limit, not waited out");
        assert!(
            text.contains("TIMEOUT stall after 61 s (limit 61 s)\n  stopped its test binary; its 2 unfinished tests run again\n"),
            "{text}"
        );
        let results: Vec<&str> = text.lines().filter(|l| l.starts_with("test result:")).collect();
        assert_eq!(results.len(), 2, "{text}");
        assert!(results[0].starts_with("test result: FAILED. 1 passed; 1 failed; 0 ignored;"), "a, and the timeout: {text}");
        assert!(results[1].starts_with("test result: FAILED. 1 passed; 1 failed; 0 ignored;"), "c, and b again: {text}");
        assert!(text.contains("---- b stdout ----\nb went wrong"), "b's failure shows its output: {text}");
        assert_eq!(dir.read("args").lines().collect::<Vec<_>>(), ["", "--list", "--exact b c"], "listed once, the rest named");
        let times = dir.read("times");
        assert!(times.lines().any(|l| l.starts_with("61.") && l.ends_with("\tpkg\tstall\tTIMEOUT")), "{times}");
        assert!(!text.contains("FLAKY"), "{text}");
    }

    /// A test that failed before the stop and passes when run again is
    /// named, and the run still fails.
    #[test]
    fn a_failure_that_passes_when_run_again_is_flaky() {
        let dir = Scratch::new("flaky");
        let binary = dir.script("libtest", LIBTEST);
        std::fs::write(dir.0.join("b-passes"), b"").unwrap();
        let out = Output::default();
        let code = watch(&binary, "default 61\n", dir.stop_by_file(), &out).run(&[]);
        let text = out.text();
        assert_eq!(code, FAILED, "{text}");
        assert!(text.contains("test result: ok. 2 passed; 0 failed;"), "{text}");
        assert!(text.contains("FLAKY b: failed before its binary was stopped, then passed when run again"), "{text}");
    }

    /// Where the stop is refused (a sandbox that refuses the signal), the
    /// stalled binary is left to end on its own and said so, and the rest
    /// still run.
    #[test]
    fn where_a_stop_is_refused_the_rest_still_run() {
        let dir = Scratch::new("refused");
        let binary = dir.script("libtest", LIBTEST);
        let out = Output::default();
        let started = Instant::now();
        let code = watch(&binary, "default 61\n", Box::new(|_: &mut Child, _| false), &out).run(&[]);
        let text = out.text();
        assert_eq!(code, FAILED, "{text}");
        assert!(started.elapsed() < Duration::from_secs(10), "the binary left running holds nothing up");
        assert!(text.contains("the sandbox refused the stop, so its test binary (pid "), "{text}");
        assert!(text.contains(") is left to end on its own; its 2 unfinished tests run again\n"), "{text}");
        assert!(text.contains("test c ... ok"), "{text}");
    }

    /// With one test thread libtest names each test as it starts, so the
    /// clock runs from there and a limit under a minute holds; what a test's
    /// child prints after the name doesn't lose it.
    #[test]
    fn with_one_thread_the_clock_starts_with_the_test() {
        let dir = Scratch::new("serial");
        let binary = dir.script(
            "libtest",
            r#"
echo "$*" >> "$dir/args"
case " $* " in
*" --list "*) printf 'one: test\nslow: test\nthree: test\n' ;;
*" --exact "*) printf '\nrunning 1 test\ntest three ... ok\n\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n\n' ;;
*)
  printf '\nrunning 3 tests\ntest one ... ok\ntest slow ... '
  echo "what its own child printed"
  while [ -d "$dir" ] && [ ! -e "$dir/stop" ]; do sleep 0.05; done
  exit 137 ;;
esac
"#,
        );
        let out = Output::default();
        let started = Instant::now();
        let code = watch(&binary, "default 1\n", dir.stop_by_file(), &out).run(&strings(&["--test-threads", "1"]));
        let text = out.text();
        assert_eq!(code, FAILED, "{text}");
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(text.contains("TIMEOUT slow after 1 s (limit 1 s)"), "{text}");
        assert!(text.contains("test three ... ok"), "{text}");
        assert_eq!(dir.read("args").lines().last(), Some("--test-threads 1 --exact three"));
    }

    /// A binary that isn't libtest's (a doctest's) passes through with its
    /// own exit code, and has the default limit for its whole run; a
    /// listing passes through untouched.
    #[test]
    fn a_binary_that_isnt_libtests_has_one_limit() {
        let dir = Scratch::new("plain");
        let out = Output::default();
        let plain = dir.script("plain", "echo hello\nexit 3\n");
        assert_eq!(watch(&plain, "default 5\n", dir.stop_by_file(), &out).run(&[]), 3);
        assert_eq!(out.text(), "hello\n");

        let out = Output::default();
        let hangs = dir.script(
            "hangs",
            "printf waiting\nwhile [ -d \"$dir\" ] && [ ! -e \"$dir/stop\" ]; do sleep 0.05; done\n",
        );
        let started = Instant::now();
        assert_eq!(watch(&hangs, "default 1\n", dir.stop_by_file(), &out).run(&[]), FAILED);
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(out.text().starts_with("waiting\nTIMEOUT pkg ("), "its own line: {}", out.text());
        assert!(out.text().ends_with("hangs): still running after 1 s, its limit\n"), "{}", out.text());

        // Its output closed, it still runs: its clock still runs too.
        let out = Output::default();
        let closes = dir.script(
            "closes",
            "exec >&-\nwhile [ -d \"$dir\" ] && [ ! -e \"$dir/stop\" ]; do sleep 0.05; done\n",
        );
        std::fs::remove_file(dir.0.join("stop")).unwrap();
        let started = Instant::now();
        assert_eq!(watch(&closes, "default 1\n", dir.stop_by_file(), &out).run(&[]), FAILED);
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(out.text().ends_with("closes): still running after 1 s, its limit\n"), "{}", out.text());

        let out = Output::default();
        let lists = dir.script("lists", "sleep 2; printf 'a: test\\n'\n");
        assert_eq!(watch(&lists, "default 1\n", dir.stop_by_file(), &out).run(&strings(&["--list"])), 0);
        assert_eq!(out.text(), "a: test\n", "unwatched, so not limited either");
    }

    /// A real libtest binary, built here with one test thread: its stalled
    /// test is caught by its own name, and the others still pass.
    #[test]
    fn a_real_libtest_binary() {
        let dir = Scratch::new("real");
        let src = dir.0.join("t.rs");
        let stop = dir.0.join("stop");
        let dir_path = &dir.0;
        std::fs::write(
            &src,
            format!(
                "#[test] fn first() {{}}\n\
                 #[test] fn second_stalls() {{ loop {{ if std::path::Path::new({stop:?}).exists() || !std::path::Path::new({dir_path:?}).exists() {{ std::process::exit(137) }} std::thread::sleep(std::time::Duration::from_millis(50)); }} }}\n\
                 #[test] fn third() {{}}\n"
            ),
        )
        .unwrap();
        let binary = dir.0.join("t");
        let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
        let built = Command::new(rustc).args(["--edition", "2021", "--test", "-o"]).arg(&binary).arg(&src).status().unwrap();
        assert!(built.success());
        let out = Output::default();
        let code = watch(&binary, "default 2\n", dir.stop_by_file(), &out).run(&strings(&["--test-threads=1"]));
        let text = out.text();
        assert_eq!(code, FAILED, "{text}");
        assert!(text.contains("test first ... ok"), "{text}");
        assert!(text.contains("TIMEOUT second_stalls after 2 s (limit 2 s)"), "{text}");
        assert!(text.contains("its 1 unfinished test runs again"), "{text}");
        assert!(text.contains("test third ... ok"), "{text}");
    }
}
