//! Kernel integration tests. Each scenario boots the kernel in QEMU,
//! reads frames off the virtio-console socket, and asserts on the
//! decoded `Frame` sequence. See `plans/kernel-integration-tests.md`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};

use itest_harness::{CpuProfile, ItestLock, LockError, RunnerConfig};

use crate::qemu;

/// Per-repo location of the flake-rate baseline. Lives in repo root so
/// PR diffs surface baseline changes alongside the changes that
/// motivated them.
pub(crate) const BASELINE_PATH: &str = ".itest-baseline.toml";

/// Per-checkout integration-test lock. Lives at the repo root (with a
/// `.itest.lock` entry in `.gitignore`) so it's easy to find and
/// inspect — `cat .itest.lock` shows the PID of the current holder.
const LOCK_PATH: &str = ".itest.lock";

/// Per-checkout root for per-run history directories (tier 2 NDJSON +
/// tier 3 log copies). Each itest run creates a timestamped
/// subdirectory under here. Gitignored.
pub(crate) const HISTORY_ROOT: &str = ".itest-runs";

/// Default OTLP receiver targeted by `baseline push` (no value) and the
/// end-of-run auto-push. Matches `stack/docker-compose.yml`'s
/// Prometheus container started with `--web.enable-otlp-receiver`.
pub(crate) const DEFAULT_OTLP_ENDPOINT: &str = "http://127.0.0.1:9090/api/v1/otlp";

/// Set by the SIGINT handler. First Ctrl-C trips this to `true`; the
/// runner sees it at the next iteration boundary and writes a
/// partial baseline. Second Ctrl-C in the handler exits the process
/// directly.
static INTERRUPT: AtomicBool = AtomicBool::new(false);

fn install_ctrlc_handler() {
    let _ = ctrlc::set_handler(|| {
        // swap returns the previous value. If it was already true,
        // this is the second Ctrl-C → force-quit.
        if INTERRUPT.swap(true, Ordering::SeqCst) {
            eprintln!("\n(second Ctrl-C — force-quit; pending baseline NOT written)");
            std::process::exit(130);
        } else {
            eprintln!(
                "\n(Ctrl-C — finishing current iteration; \
                 partial baseline will be written if --update-baseline. \
                 Press Ctrl-C again to force-quit.)"
            );
        }
    });
}

/// Print a failed itest capture's frame transcript (`.itest-runs/`), so a
/// capture can be inspected without hand-parsing JSON. Defaults to the latest
/// run and the first capture in it; `--scenario`/`--tail`/`--grep` narrow it.
pub fn show(run: Option<&str>, scenario: Option<&str>, tail: Option<usize>, grep: Option<&str>) -> ExitCode {
    let dir = match run {
        Some(r) => PathBuf::from(HISTORY_ROOT).join(r),
        None => match latest_run_dir() {
            Some(d) => d,
            None => {
                eprintln!("no runs under {HISTORY_ROOT}/");
                return ExitCode::FAILURE;
            }
        },
    };
    let Some(cap_path) = find_capture(&dir, scenario) else {
        eprintln!("no .capture.json in {}", dir.display());
        return ExitCode::FAILURE;
    };
    let capture = match itest_harness::load_capture(&cap_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("load {}: {e}", cap_path.display());
            return ExitCode::FAILURE;
        }
    };
    eprintln!(
        "# {} — outcome={:?}, frames_seen={}",
        cap_path.display(),
        capture.outcome,
        capture.frames_seen
    );
    let frames: Vec<&String> = capture
        .transcript
        .iter()
        .filter(|f| grep.is_none_or(|g| f.contains(g)))
        .collect();
    let start = tail.map_or(0, |n| frames.len().saturating_sub(n));
    for f in &frames[start..] {
        println!("{f}");
    }
    ExitCode::SUCCESS
}

/// The most recent run directory under [`HISTORY_ROOT`] — timestamp names sort
/// lexicographically, so the max is the latest. `None` if there are no runs.
fn latest_run_dir() -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(HISTORY_ROOT)
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    dirs.pop()
}

/// The first `*.capture.json` in `dir` (optionally filtered to a scenario name).
fn find_capture(dir: &Path, scenario: Option<&str>) -> Option<PathBuf> {
    let mut caps: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(".capture.json") && scenario.is_none_or(|s| n.contains(s)))
        })
        .collect();
    caps.sort();
    caps.into_iter().next()
}

pub(crate) mod baseline;
mod harness;
mod matchers;
mod scenarios;

use itest_harness::{Scenario, ScenarioReport};

use self::harness::{Boot, View};

/// Placeholder `run` for catalog metadata. Scenarios execute via
/// [`scenario_view_fn`] + the `run_group` executor (which spawns a
/// [`Boot`] and runs each scenario against a [`View`]), never through
/// `Scenario::run`. The runner only calls `Scenario::run` when no executor
/// is configured, which never happens here — so this is unreachable.
fn unreached_run() -> Result<(), String> {
    unreachable!("xtask scenarios run via the run_group executor / scenario_view_fn, not Scenario::run")
}

/// Build the integration-test catalog from a table of rows. Emits two
/// co-generated items so they can't drift: `const SCENARIOS` (metadata for
/// the runner — the `run` field is the never-called [`unreached_run`]
/// placeholder) and `fn scenario_view_fn(name)` mapping each scenario name
/// to its `fn(&mut View)` assertion body (which the executor calls).
///
/// Row grammar: `<profile> "<name>" <fn-path> [tag, …]? {"<workload>"}? ;`
/// — `wfi` is wfi-bounded (fans out across the parallel pool), `cpu` runs
/// real guest work (a serial pass); tags feed `--tag` selection; the
/// braced workload is the `workload=` bootarg + the shared-boot grouping
/// key. (`cpu_bound` classification per plans/itest-parallel-scenarios.md.)
macro_rules! catalog {
    ( $(
        $profile:ident $name:literal $func:path
        $( [ $( $tag:ident ),* $(,)? ] )?
        $( { $wl:literal } )?
    );* $(;)? ) => {
        const SCENARIOS: &[Scenario] = &[ $(
            catalog!(@meta $profile $name)
                $( .tagged(&[ $( stringify!($tag) ),* ]) )?
                $( .on_workload($wl) )?
        ),* ];

        fn scenario_view_fn(name: &str) -> fn(&mut View) -> Result<(), String> {
            match name {
                $( $name => $func, )*
                other => panic!("no scenario fn registered for {other:?}"),
            }
        }
    };
    (@meta wfi $name:literal) => { Scenario::new($name, unreached_run) };
    (@meta cpu $name:literal) => { Scenario::cpu_bound($name, unreached_run) };
}

catalog! {
    wfi "boot-reaches-heartbeat"          scenarios::boot_reaches_heartbeat         [boot];
    wfi "heartbeat-cadence"               scenarios::heartbeat_cadence              [boot];
    wfi "pre-init-order"                  scenarios::pre_init_order                 [boot];
    wfi "kernel-runs-at-higher-half"      scenarios::kernel_runs_at_higher_half     [boot];
    wfi "frame-allocator-metrics"         scenarios::frame_allocator_metrics        [frame];
    wfi "frame-allocator-oom"             scenarios::frame_allocator_oom            [frame, oom]    {"frame-oom"};
    wfi "kernel-heap-metrics"             scenarios::kernel_heap_metrics            [heap];
    wfi "sched-context-switch-smoke"      scenarios::sched_context_switch_smoke     [sched];
    wfi "sched-spawn-registers-thread"    scenarios::sched_spawn_registers_thread   [sched];
    cpu "sched-yield-round-trips"         scenarios::sched_yield_round_trips        [sched];
    wfi "sched-spans-carry-task-id"       scenarios::sched_spans_carry_task_id      [sched];
    wfi "sched-context-switches-on-wire"  scenarios::sched_context_switches_on_wire [sched];
    wfi "sched-span-survives-yield"       scenarios::sched_span_survives_yield      [sched];
    cpu "heap-oom"                        scenarios::heap_oom                       [heap, oom]    {"heap-oom"};
    cpu "workload-cooperative-baseline"   scenarios::workload_cooperative_baseline  [workload];
    cpu "smp-producer-consumer-correctness" scenarios::smp_producer_consumer_correctness [smp, workload] {"smp burst=256"};
    wfi "ipi-self-wakeup"                 scenarios::ipi_self_wakeup                [smp, ipi];
    wfi "smp-secondary-hart-boots"        scenarios::smp_secondary_hart_boots       [smp];
    wfi "smp-spawn-on-hart-1-runs"        scenarios::smp_spawn_on_hart_1_runs       [smp];
    wfi "smp-spans-carry-hart-id"         scenarios::smp_spans_carry_hart_id        [smp];
    wfi "smp-ipi-wakes-idle-hart"         scenarios::smp_ipi_wakes_idle_hart        [smp, ipi];
    cpu "spawn-storm"                     scenarios::spawn_storm                    [smp, stress]   {"spawn-storm"};
    cpu "ipi-pong"                        scenarios::ipi_pong                       [smp, ipi, stress] {"ipi-pong"};
    cpu "shootdown-storm"                 scenarios::shootdown_storm                [smp, stress]   {"shootdown-storm"};
    cpu "smp-tlb-shootdown-visible"       scenarios::smp_tlb_shootdown_visible      [smp]           {"tlb-shootdown"};
    cpu "smp-ping-pong-cadence"           scenarios::smp_ping_pong_cadence          [smp, ipi]      {"ping-pong"};
    wfi "sched-task-exits-cleanly"        scenarios::sched_task_exits_cleanly       [sched];
    wfi "task-stack-high-water"           scenarios::task_stack_high_water_reported [sched];
    wfi "stack-overflow-detected"         scenarios::stack_overflow_detected        [sched]         {"stack-canary"};
    wfi "block-wake-smoke"                scenarios::block_wake_smoke               [sched]         {"block-wake"};
    wfi "ipc-message-crosses"             scenarios::ipc_message_crosses            [userspace, ipc] {"ipc"};
    wfi "ipc-trace-crosses"               scenarios::ipc_trace_crosses              [userspace, ipc] {"ipc"};
    wfi "ipc-telemetry"                   scenarios::ipc_telemetry                  [userspace, ipc] {"ipc"};
    wfi "ipc-wakeup-is-prompt"            scenarios::ipc_wakeup_is_prompt           [userspace, ipc] {"ipc"};
    wfi "rpc-round-trips"                 scenarios::rpc_round_trips                [userspace, ipc] {"ipc-rpc"};
    wfi "rpc-trace-nests"                 scenarios::rpc_trace_nests                [userspace, ipc] {"ipc-rpc"};
    wfi "rpc-telemetry"                   scenarios::rpc_telemetry                  [userspace, ipc] {"ipc-rpc"};
    wfi "rpc-reply-recv"                  scenarios::rpc_reply_recv                 [userspace, ipc] {"ipc-rpc"};
    wfi "badge-mint-mints-and-refuses"    scenarios::badge_mint_mints_and_refuses   [userspace, ipc] {"badge-mint"};
    wfi "badge-handout-transfers-cap"     scenarios::badge_handout_transfers_cap    [userspace, ipc] {"badge-handout"};
    wfi "badge-demux-distinguishes-clients" scenarios::badge_demux_distinguishes_clients [userspace, ipc] {"badge-handout"};
    wfi "fs-connect-mints-root"           scenarios::fs_connect_mints_root          [userspace, ipc] {"fs"};
    wfi "fs-stat-root"                    scenarios::fs_stat_root                   [userspace, ipc] {"fs"};
    wfi "fs-create-stat"                  scenarios::fs_create_then_stat            [userspace, ipc] {"fs"};
    wfi "fs-write-read"                   scenarios::fs_write_read_roundtrip        [userspace, ipc] {"fs"};
    wfi "fs-lookup-rights-gate"           scenarios::fs_lookup_rights_gate          [userspace, ipc] {"fs"};
    wfi "fs-remove"                       scenarios::fs_remove_unlinks              [userspace, ipc] {"fs"};
    wfi "fs-readdir"                      scenarios::fs_readdir_lists_entries       [userspace, ipc] {"fs"};
    wfi "fs-workload"                     scenarios::fs_workload_traces             [userspace, ipc] {"fs"};
    wfi "userspace-bad-ptr"               scenarios::userspace_bad_ptr_refused      [userspace]      {"userspace-bad-ptr"};
    wfi "userspace-custom-metric"         scenarios::userspace_custom_metric        [userspace]      {"probe"};
    wfi "span-name-not-poisonable"        scenarios::span_name_not_poisonable       [userspace]      {"probe"};
    wfi "stitch-telemetry-on-the-wire"    scenarios::stitch_telemetry_on_the_wire   [userspace, stitch] {"stitch-repl"};
    wfi "stitch-fs-loads-and-runs"        scenarios::stitch_fs_loads_and_runs       [userspace, stitch, fs] {"stitch-fs"};
    cpu "mutex-storm"                     scenarios::mutex_storm                    [smp, stress]   {"mutex-storm"};
    cpu "virtio-storm"                    scenarios::virtio_storm                   [smp, stress]   {"virtio-storm"};
    // Userspace scenarios are wfi-bounded: `hello` exits (hart 1 falls back
    // to its idle `wfi` loop) and `faulter` faults (the kernel parks the hart
    // in `wfi`). So they fan out in the parallel pool rather than a serial pass.
    // The nine `{"userspace"}` rows all boot the *same* `hello` run — a single
    // execution that grants both caps, emits telemetry=42, opens hello.work,
    // is refused a wrong-object handle, is denied an ungranted handle, yields,
    // and exits. That shared superset is what makes them one shared-boot group.
    wfi "userspace-emits-telemetry"       scenarios::userspace_emits_telemetry     [userspace]  {"userspace"};
    wfi "userspace-cannot-touch-kernel"   scenarios::userspace_cannot_touch_kernel  [userspace]  {"userspace-fault"};
    wfi "userspace-grant-snitched"        scenarios::userspace_grant_snitched       [userspace]  {"userspace"};
    wfi "userspace-cap-denied"            scenarios::userspace_cap_denied           [userspace]  {"userspace"};
    wfi "userspace-cap-granted-event"     scenarios::userspace_cap_granted_event    [userspace]  {"userspace"};
    wfi "userspace-process-exits"         scenarios::userspace_process_exits        [userspace]  {"userspace"};
    wfi "userspace-yield-round-trips"     scenarios::userspace_yield_round_trips     [userspace]  {"userspace"};
    wfi "userspace-spansink-granted"      scenarios::userspace_spansink_granted     [userspace]  {"userspace"};
    wfi "userspace-emits-span"            scenarios::userspace_emits_span           [userspace]  {"userspace"};
    wfi "userspace-prints"                scenarios::userspace_prints               [userspace]  {"userspace"};
    wfi "userspace-refusal-snitched"      scenarios::userspace_refusal_snitched     [userspace]  {"userspace"};
    wfi "userspace-quota-refused"         scenarios::userspace_quota_refused        [userspace]  {"userspace-span-flood"};
    cpu "two-userspace-workers-round-robin" scenarios::two_userspace_workers_round_robin [userspace] {"workers"};
    wfi "heap-grows-on-demand"            scenarios::heap_grows_on_demand           [userspace]  {"heap-grow"};
    cpu "preempt-runaway-user-task"       scenarios::preempt_runaway_user_task      [userspace]  {"user-hog"};
    cpu "preemption-telemetry"            scenarios::preemption_telemetry           [userspace]  {"user-hog"};
    cpu "syscall-hog-still-preempted"     scenarios::syscall_hog_still_preempted    [userspace]  {"syscall-hog"};
    cpu "console-echo-round-trips"        scenarios::console_echo_round_trips       [userspace]  {"console-echo"};
    cpu "spawn-delegates-to-child"        scenarios::spawn_delegates_to_child       [userspace]  {"spawn-demo"};
    cpu "spawn-transfer-links-to-parent"  scenarios::spawn_transfer_links_to_parent [userspace]  {"spawn-demo"};
    cpu "spawn-reclaims-memory"           scenarios::spawn_reclaims_memory          [userspace]  {"spawn-reap"};
    cpu "wait-any-reaps-exiting-child"    scenarios::wait_any_reaps_the_exiting_child [userspace] {"wait-any"};
    cpu "init-supervises-a-child"         scenarios::init_supervises_a_child        [userspace]  {"init"};
    cpu "init-brings-up-fs-server"        scenarios::init_brings_up_fs_server       [userspace]  {"init"};
    cpu "endpoint-create-yields-owning-cap" scenarios::endpoint_create_yields_an_owning_cap [userspace] {"endpoint-create"};
    cpu "notify-signal-wakes-waiter"      scenarios::notify_signal_wakes_waiter     [userspace]  {"notify-smoke"};
    cpu "priorities-ordered-but-fair"     scenarios::priorities_ordered_but_fair    [userspace]  {"priorities"};
}

/// Set the process-wide failure-capture transcript depth. Call once at
/// startup, before `run`. Delegates to the harness, which reads it at
/// every `Harness::spawn`.
pub fn set_capture_level(level: itest_harness::CaptureLevel) {
    harness::set_capture_level(level);
}

/// Options for one `run` — the `itest` subcommand's flags, grouped so the
/// entry point takes a single value instead of ten positional arguments.
pub struct RunConfig {
    /// Scenario name or comma-separated list; `None` runs every scenario.
    pub name: Option<String>,
    /// Number of full passes to perform — surfaces flaky scenarios.
    pub repeat: u32,
    /// Bypass the integration-test lock (use only if it's known stale).
    pub force: bool,
    /// After the run, write each scenario's results back to the baseline
    /// (previous `current` pushed to `history`).
    pub update_baseline: bool,
    /// Abort the repeat sweep once cumulative failures reach this count.
    pub fail_fast: Option<u32>,
    /// Push the canonical baseline to OTLP when the run finishes.
    pub auto_push: bool,
    /// Parallel workers for the wfi-bounded scenario batch.
    pub jobs: u32,
    /// Parallel workers for the cpu-bound scenario batch.
    pub cpu_jobs: u32,
    /// Restrict the run to one CPU-profile class.
    pub profile_filter: Option<CpuProfile>,
    /// Scenario names to exclude (applied after `profile_filter`).
    pub skip: Vec<String>,
    /// Select scenarios carrying any of these tags (union). Mutually
    /// exclusive with `name`. Empty = no tag filtering.
    pub tags: Vec<String>,
    /// Shared-boot mode: group scenarios by `workload` and run each group
    /// against one kernel boot. `false` = separate boots (the flake gate).
    pub shared: bool,
}

/// Entry point from `main`: select scenarios per `config`, run them in QEMU
/// (optionally repeating `config.repeat` times), and compare against the
/// baseline. `config.name` picks scenarios (`None` = all).
pub fn run(config: RunConfig) -> ExitCode {
    let RunConfig {
        name,
        repeat,
        force,
        update_baseline,
        fail_fast,
        auto_push,
        jobs,
        cpu_jobs,
        profile_filter,
        skip,
        tags,
        shared,
    } = config;
    if !qemu_available() {
        eprintln!("xtask test: qemu-system-riscv64 not on PATH — skipping");
        return ExitCode::SUCCESS;
    }

    // Acquire the integration-test lock. `--force` bypasses; otherwise
    // any contender (concurrent invocation from another terminal, agent,
    // or CI job on the same checkout) gets rejected here with the
    // holder's PID. The guard is held until `run` returns.
    let _lock_guard = if force {
        None
    } else {
        match ItestLock::acquire(Path::new(LOCK_PATH)) {
            Ok(guard) => Some(guard),
            Err(LockError::AlreadyHeld { pid }) => {
                eprintln!("error: {}", LockError::AlreadyHeld { pid });
                eprintln!("       Pass --force if you know the lock is stale.");
                return ExitCode::from(2);
            }
            Err(LockError::Io(e)) => {
                eprintln!("error: failed to acquire itest lock at {LOCK_PATH}: {e}");
                return ExitCode::from(2);
            }
        }
    };

    // Warn (but don't kill) about pre-existing qemus. The lock above
    // already prevents itest-vs-itest races; the remaining concern is a
    // user's `xtask boot` / `xtask debug` / manual QEMU running in
    // parallel. We surface the situation rather than silently murder it.
    let stale = detect_stale_qemus();
    if !stale.is_empty() {
        eprintln!(
            "warning: {} stale qemu-system-riscv64 process(es) detected (pid {}).",
            stale.len(),
            stale.iter().map(u32::to_string).collect::<Vec<_>>().join(", ")
        );
        eprintln!(
            "         Probably from `xtask boot`/`xtask debug` or a manual invocation."
        );
        eprintln!(
            "         Cross-test interference is possible. Kill them manually if needed."
        );
    }

    // `--tag` and a positional name select scenarios two different ways;
    // combining them is ambiguous, so reject it rather than guess an
    // intersection-vs-union. Each invocation picks one selection mode.
    if !tags.is_empty() && name.is_some() {
        eprintln!("error: --tag cannot be combined with a positional scenario name");
        return ExitCode::from(2);
    }
    // Base selection: by name (or all) when no `--tag` is given, else
    // by tag (union over the full catalog). `--profile` / `--skip`
    // filter whatever this produces.
    let to_run: Vec<&Scenario> = if tags.is_empty() {
        match name.as_deref() {
        // One name, or a comma-separated list (`itest a,b,c`).
        // Whitespace around each name is trimmed; any unknown name is a
        // hard error — a typo shouldn't silently run a subset.
        Some(n) => {
            let mut selected = Vec::new();
            for part in n.split(',').map(str::trim).filter(|p| !p.is_empty()) {
                let Some(s) = SCENARIOS.iter().find(|s| s.name == part) else {
                    eprintln!("unknown scenario: {part}");
                    eprintln!("known: {}", SCENARIOS.iter().map(|s| s.name).collect::<Vec<_>>().join(", "));
                    return ExitCode::from(2);
                };
                selected.push(s);
            }
            if selected.is_empty() {
                eprintln!("no scenarios selected (empty list?)");
                return ExitCode::from(2);
            }
            selected
        }
        None => SCENARIOS.iter().collect(),
        }
    } else {
        let all: Vec<&Scenario> = SCENARIOS.iter().collect();
        match itest_harness::select_by_tags(&all, &tags) {
            Ok(selected) => {
                eprintln!("--tag: {} scenario(s) selected", selected.len());
                selected
            }
            Err(msg) => {
                eprintln!("{msg}");
                return ExitCode::from(2);
            }
        }
    };
    let to_run: Vec<&Scenario> = match profile_filter {
        Some(p) => {
            let label = match p {
                CpuProfile::Wfi => "wfi",
                CpuProfile::Cpu => "cpu",
            };
            let filtered: Vec<&Scenario> =
                to_run.into_iter().filter(|s| s.cpu_profile == p).collect();
            if filtered.is_empty() {
                eprintln!("--profile {label}: no scenarios match this filter");
                return ExitCode::from(2);
            }
            eprintln!("--profile {label}: {} scenarios selected", filtered.len());
            filtered
        }
        None => to_run,
    };
    let to_run: Vec<&Scenario> = if skip.is_empty() {
        to_run
    } else {
        // Warn on unknown skip names — usually a typo, and silently
        // skipping nothing would hide it.
        for s in &skip {
            if !SCENARIOS.iter().any(|sc| sc.name == s.as_str()) {
                eprintln!("warning: --skip {s:?}: no such scenario (ignored)");
            }
        }
        let before = to_run.len();
        let filtered: Vec<&Scenario> = to_run
            .into_iter()
            .filter(|sc| !skip.iter().any(|s| s == sc.name))
            .collect();
        eprintln!("--skip: excluded {} scenario(s)", before - filtered.len());
        if filtered.is_empty() {
            eprintln!("--skip: all selected scenarios were excluded; nothing to run");
            return ExitCode::from(2);
        }
        filtered
    };

    // Hook closures. None of these escape the call — the lifetime
    // parameter on RunnerConfig keeps them bounded to this scope.
    // One build for the whole suite: the `itest-workloads` kernel.
    // With no `workload=` bootarg it runs the exact default demo
    // (additive guarantee), so default-demo scenarios use it as-is;
    // workload scenarios select via QEMU `-append`. No per-scenario
    // rebuilds. See `docs/runtime-workload-selection-design.md`.
    let build = || match qemu::build_kernel(&["itest-workloads"]) {
        // A non-zero exit (e.g. a compile error) MUST abort the run —
        // otherwise the suite silently runs the previously-built (stale)
        // kernel and reports bogus pass/fail. `map(|_| ())` used to drop
        // the status and hide exactly that.
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(format!("kernel build failed ({status})")),
        Err(e) => Err(e.to_string()),
    };
    let commit_for = current_commit_short;

    // The executor: spawn one `Boot` for the group (separate mode = one
    // scenario; shared mode = the same-workload group), then run each
    // scenario's `fn(&mut View)` against a fresh `View` over that boot and
    // package the View's `max_wait()` / `take_capture()` + the boot's log
    // path into a `ScenarioReport`. All scenarios in a group share a
    // workload (the runner's grouping invariant), so the group's bootarg
    // is the first member's.
    let run_group = |scns: &[&Scenario]| -> Vec<ScenarioReport> {
        let Some(first) = scns.first() else { return Vec::new() };
        let boot = match Boot::spawn(first.name, first.workload) {
            Ok(boot) => boot,
            // Spawn failure is infra, not a scenario assertion: report it
            // for every member so the runner records each as failed.
            Err(e) => {
                return scns
                    .iter()
                    .map(|_| ScenarioReport {
                        result: Err(format!("boot spawn failed: {e}")),
                        max_wait: None,
                        capture: None,
                        log_path: None,
                    })
                    .collect();
            }
        };
        let log_path = boot.log_path();
        scns.iter()
            .map(|s| {
                let mut view = boot.view();
                let result = scenario_view_fn(s.name)(&mut view);
                ScenarioReport {
                    result,
                    max_wait: Some(view.max_wait()),
                    capture: view.take_capture(),
                    log_path: Some(log_path.clone()),
                }
            })
            .collect()
    };

    // Install the SIGINT handler before constructing config — the
    // INTERRUPT flag is what the runner reads at iteration boundaries.
    install_ctrlc_handler();

    let config = RunnerConfig {
        one_shot_build: Some(&build),
        run_group: Some(&run_group),
        current_commit: Some(&commit_for),
        baseline_file: Some(PathBuf::from(BASELINE_PATH)),
        fail_fast,
        pending_baseline: Some(PathBuf::from(format!("{BASELINE_PATH}.pending"))),
        interrupt: Some(&INTERRUPT),
        history_root: Some(PathBuf::from(HISTORY_ROOT)),
        jobs,
        cpu_jobs,
        invocation: Some(std::env::args().collect::<Vec<_>>().join(" ")),
        shared,
    };

    let outcome = itest_harness::run(&to_run, repeat, update_baseline, &config);

    if auto_push {
        try_auto_push();
    }

    outcome
}

/// Probe the bundled stack's OTLP receiver and push the canonical
/// baseline if it answers. Warn (don't be silent) when it doesn't —
/// the user opted in to live metrics by enabling auto-push (it's on
/// by default), so silent skipping would hide the misconfiguration.
///
/// Bounded by a short connect timeout so a missing stack costs ~1s
/// at the end of a run, not ureq's default ~75s.
fn try_auto_push() {
    let endpoint = DEFAULT_OTLP_ENDPOINT;
    let timeouts = Some((
        std::time::Duration::from_secs(1),
        std::time::Duration::from_secs(3),
    ));
    match baseline::load_and_push(endpoint, timeouts) {
        // No baseline yet (e.g. first run on a fresh checkout) — silent skip.
        Ok(None) => {}
        Ok(Some((status, scenarios))) if (200..300).contains(&status) => {
            eprintln!("auto-push: pushed {scenarios} scenarios to {endpoint} (HTTP {status})");
        }
        Ok(Some((status, _))) => {
            eprintln!(
                "auto-push: OTLP receiver at {endpoint} returned HTTP {status}.\n\
                 Confirm the stack is healthy, or pass --no-auto-push to silence."
            );
        }
        Err(e) => {
            eprintln!(
                "auto-push: skipped ({e}).\n\
                 Run `cargo xtask stack up`, or pass --no-auto-push to silence."
            );
        }
    }
}

/// Returns the short commit hash for HEAD via `git rev-parse`. None on
/// any failure (not in a git checkout, git missing, etc.). The baseline
/// file falls back to "unknown" in that case.
fn current_commit_short() -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?;
    let trimmed = s.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Workspace crates that have host-runnable `cargo test` suites.
/// The kernel itself is `no_std`/`no_main` and won't build for the
/// host — testable logic lives in `kernel-core`. Each entry is the
/// crate name plus any extra args (features) the suite needs.
const UNIT_TEST_CRATES: &[(&str, &[&str])] = &[
    ("kernel-core", &[]),
    ("protocol", &["--features", "std"]),
    ("collector", &[]),
    ("itest-harness", &[]),
];

/// Run every workspace crate's host unit tests, in order. Returns
/// `SUCCESS` only if all crates pass. Bails out on first failure
/// (no point continuing if `kernel-core` is broken).
pub fn run_unit_tests() -> ExitCode {
    eprintln!("=== unit tests ===");
    for (crate_name, extra_args) in UNIT_TEST_CRATES {
        let mut args = vec!["test", "-p", crate_name, "--quiet"];
        args.extend_from_slice(extra_args);
        if !run_cargo_test(crate_name, &args, &[]) {
            return ExitCode::from(1);
        }
    }
    // The loom model-check tests (kernel-core/tests/loom_tx.rs) live
    // behind `--cfg loom`, where loom swaps in its own Mutex/thread/
    // UnsafeCell. They need a separate compilation with that cfg set; a
    // normal `cargo test` compiles the file to nothing. The config-level
    // rustflags are riscv-target-scoped, so overriding RUSTFLAGS for this
    // host build clobbers nothing.
    if !run_cargo_test(
        "kernel-core (loom)",
        &["test", "-p", "kernel-core", "--test", "loom_tx", "--quiet"],
        &[("RUSTFLAGS", "--cfg loom")],
    ) {
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}

/// Run one `cargo test` invocation, printing `ok`/`FAILED` for `label`.
/// On failure surfaces the captured stderr so the user needn't re-run
/// with `--verbose`. `env` overrides are applied to the child (e.g.
/// `RUSTFLAGS=--cfg loom`). Returns `true` iff the suite passed.
fn run_cargo_test(label: &str, args: &[&str], env: &[(&str, &str)]) -> bool {
    eprint!("  {label} ... ");
    let mut cmd = std::process::Command::new("cargo");
    cmd.args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    for (key, value) in env {
        cmd.env(key, value);
    }
    match cmd.output() {
        Ok(out) if out.status.success() => {
            eprintln!("ok");
            true
        }
        Ok(out) => {
            eprintln!("FAILED");
            let stderr = String::from_utf8_lossy(&out.stderr);
            for line in stderr.lines() {
                eprintln!("    {line}");
            }
            false
        }
        Err(e) => {
            eprintln!("FAILED to invoke cargo: {e}");
            false
        }
    }
}

fn qemu_available() -> bool {
    std::process::Command::new("qemu-system-riscv64")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Wipe out any `qemu-system-riscv64` processes already on the host.
/// Run before the suite by default — a stale QEMU from `cargo xtask
/// boot`, a debug session, or a previous interrupted suite would
/// compete for host CPU and cause spurious flakes. Bypassed with
/// `--keep-existing-qemus`.
/// Return the PIDs of currently-running `qemu-system-riscv64`
/// processes. The integration-test lock prevents itest-vs-itest races
/// directly; this detection covers the remaining case of `xtask boot`,
/// `xtask debug`, or a manually-launched QEMU sharing the machine.
fn detect_stale_qemus() -> Vec<u32> {
    std::process::Command::new("pgrep")
        .arg("qemu-system-riscv64")
        .output()
        .ok()
        .map(|o| {
            std::str::from_utf8(&o.stdout)
                .map(|s| s.lines().filter_map(|l| l.trim().parse().ok()).collect())
                .unwrap_or_default()
        })
        .unwrap_or_default()
}
