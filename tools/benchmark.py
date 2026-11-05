"""Compare Umi-OCR and the Rust OCR service against generated local fixtures."""

import argparse
import base64
import ctypes
from ctypes import wintypes
import hashlib
import io
import json
import math
import os
from pathlib import Path
import platform
import random
import shutil
import socket
import statistics
import subprocess
import tempfile
import threading
import time
import unicodedata
import urllib.error
import urllib.request

from PIL import Image, ImageDraw, ImageFont
import psutil


PROJECT = Path(__file__).resolve().parents[1]
OCR_OPTIONS = {
    "ocr.language": "models/config_chinese.txt",
    "ocr.cls": False,
    "ocr.limit_side_len": 960,
    "tbpu.parser": "none",
    "tbpu.ignoreArea": [],
    "data.format": "dict",
}
MIB = 1024 * 1024
SAMPLE_INTERVAL = 0.025
CPU_PEAK_WINDOW = 0.25
HTTP = urllib.request.build_opener(urllib.request.ProxyHandler({}))


class StartupError(RuntimeError):
    pass


def fixture_specs():
    return [
        ("compact", (540, 160), 32, ["离线文字识别", "本地测试 ABC 123"], "black", "white"),
        ("english", (820, 270), 37, ["Offline OCR benchmark", "Invoice number 20261106", "The quick brown fox jumps."], "black", "white"),
        ("chinese", (960, 440), 39, ["文字识别对照样本", "项目状态：编译成功", "截图坐标和文本置信度", "云端与本地计算", "订单编号 3618027"], "black", "white"),
        ("dense", (1600, 900), 47, [f"第{line:02}行　系统功能和性能测试　OCR {3072 + line * 513}" for line in range(1, 12)], "black", "white"),
        ("dim", (920, 280), 41, ["灰色文字测试", "可读性与文本检测", "Contrast level 42%"], "#777777", "#eeeeee"),
        ("tiny_noise", (680, 220), 18, ["账户余额 ￥1284.76", "项目 0O1Il-8B5S", "混合字形：截取與識別", "參數版本 PP-OCR v3/v6"], "#56595b", "#dedbd2"),
        ("tilted", (850, 340), 32, ["倾斜票据 316.82 元", "款项状态：已支付", "REF A8W-9235-OM"], "#202020", "white"),
        ("blank", (900, 450), 31, [], "black", "white"),
    ]


def make_fixtures(output):
    font_file = Path(os.environ["WINDIR"]) / "Fonts" / "msyh.ttc"
    fixtures = []
    for name, size, pixels, lines, color, background in fixture_specs():
        image = Image.new("RGB", size, background)
        draw = ImageDraw.Draw(image)
        font = ImageFont.truetype(str(font_file), pixels)
        for index, line in enumerate(lines):
            draw.text((30, 18 + index * int(pixels * 1.48)), line, fill=color, font=font)
        if name == "tiny_noise":
            rng = random.Random(613)
            for _ in range(3700):
                x, y = rng.randrange(size[0]), rng.randrange(size[1])
                red, green, blue = image.getpixel((x, y))
                offset = rng.randrange(-50, 40)
                draw.point((x, y), fill=tuple(min(255, max(0, component + offset)) for component in (red, green, blue)))
        if name == "tilted":
            image = image.rotate(-8, resample=Image.Resampling.BICUBIC, expand=True, fillcolor=background)
        image_data = io.BytesIO()
        image.save(image_data, format="PNG")
        png = image_data.getvalue()
        if output is not None:
            output.mkdir(parents=True, exist_ok=True)
            (output / f"{name}.png").write_bytes(png)
        fixtures.append({
            "name": name,
            "size": image.size,
            "expected": "\n".join(lines),
            "sha256": hashlib.sha256(png).hexdigest(),
            "base64": base64.b64encode(png).decode(),
        })
    return fixtures


def hash_file(path):
    with path.open("rb") as file:
        return hashlib.file_digest(file, "sha256").hexdigest()


def freeze_executable(source, target):
    frozen = target / source.name
    shutil.copy2(source, frozen)
    sidecars = sorted(source.parent.glob("*.dll"))
    for sidecar in sidecars:
        shutil.copy2(sidecar, target)
    return frozen, {path.name: hash_file(target / path.name) for path in sidecars}


def reserve_port():
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


def stage_umi(source, target, port, threads):
    target.mkdir()
    shutil.copy2(source / "Umi-OCR.exe", target)
    data = target / "UmiOCR-data"
    shutil.copytree(
        source / data.name, data,
        ignore=shutil.ignore_patterns(".settings", ".pre_settings", "logs", "temp_doc", "__pycache__"),
    )
    (data / ".pre_settings").write_text(json.dumps({
        "i18n": "", "opengl": "AA_UseSoftwareOpenGL", "server_port": port, "last_pid": -1, "last_ptime": -1,
    }), encoding="utf-8")
    (data / ".settings").write_text(
        "[Global]\n"
        "window.startupInvisible=true\n"
        f"server.port={port}\n"
        f"ocr.win7_x64_PaddleOCR-json.cpu_threads={threads}\n"
        "ocr.win7_x64_PaddleOCR-json.enable_mkldnn=true\n"
        "ocr.win7_x64_PaddleOCR-json.ram_max=-1\n"
        "ocr.win7_x64_PaddleOCR-json.ram_time=-1\n"
        "[ScreenshotOCR]\nhotkey.screenshot=\nhotkey.paste=\nhotkey.reScreenshot=\n",
        encoding="ascii",
    )


class ProcessSampler:
    def __init__(self, pid):
        self.root = psutil.Process(pid)
        self.known = {self.root: self.root}
        self.cpu = {}
        self.lock = threading.Lock()
        self.samples = []
        self.stop = threading.Event()
        self.thread = threading.Thread(target=self.run, daemon=True)
        self.thread.start()

    def read(self):
        with self.lock:
            start = time.perf_counter()
            processes = []
            try:
                processes = [self.root, *self.root.children(recursive=True)]
            except psutil.NoSuchProcess:
                pass
            entries = []
            for process in processes:
                self.known[process] = process
                try:
                    with process.oneshot():
                        mem = process.memory_info()
                        times = process.cpu_times()
                        self.cpu[process] = (times.user, times.system)
                        entries.append({"pid": process.pid, "name": process.name(), "ws_mib": mem.rss / MIB,
                                        "private_mib": mem.private / MIB,
                                        "cpu_user_seconds": times.user, "cpu_kernel_seconds": times.system})
                except (psutil.NoSuchProcess, psutil.AccessDenied):
                    pass
            # Keep the last counters of exited descendants so the process-tree total stays cumulative.
            return {
                "monotonic_seconds": (start + time.perf_counter()) / 2,
                "ws_mib": round(sum(item["ws_mib"] for item in entries), 3),
                "private_mib": round(sum(item["private_mib"] for item in entries), 3),
                "cpu_user_seconds": sum(user for user, _ in self.cpu.values()),
                "cpu_kernel_seconds": sum(kernel for _, kernel in self.cpu.values()),
                "processes": entries,
            }

    def run(self):
        while not self.stop.is_set():
            self.samples.append(self.read())
            self.stop.wait(SAMPLE_INTERVAL)

    def close(self):
        self.stop.set()
        self.thread.join()
        self.read()
        # Keep psutil's PID/creation-time identity checks before stopping any descendants.
        for process in self.known.values():
            try:
                process.kill()
            except (psutil.NoSuchProcess, psutil.AccessDenied):
                pass
        psutil.wait_procs(self.known.values(), timeout=5)


def cpu_interval(start, end):
    elapsed = end["monotonic_seconds"] - start["monotonic_seconds"]
    user = max(0.0, end["cpu_user_seconds"] - start["cpu_user_seconds"])
    kernel = max(0.0, end["cpu_kernel_seconds"] - start["cpu_kernel_seconds"])
    total = user + kernel
    cores = total / elapsed
    return {"wall_seconds": elapsed, "cpu_user_seconds": user, "cpu_kernel_seconds": kernel,
            "cpu_total_seconds": total, "average_logical_cores": cores,
            "average_machine_percent": 100 * cores / (os.cpu_count() or 1)}


def warm_cpu_peak(samples, start, end):
    samples = [start, *(sample for sample in samples if
                        start["monotonic_seconds"] < sample["monotonic_seconds"] < end["monotonic_seconds"]), end]
    left = 0
    peak = 0.0
    for right, last in enumerate(samples[1:], 1):
        cutoff = last["monotonic_seconds"] - CPU_PEAK_WINDOW
        while left + 1 < right and samples[left + 1]["monotonic_seconds"] <= cutoff:
            left += 1
        if samples[left]["monotonic_seconds"] <= cutoff:
            peak = max(peak, cpu_interval(samples[left], last)["average_logical_cores"])
    return peak


def request(port, path, body=None, timeout=120):
    headers = {"Content-Type": "application/json"} if body is not None else {}
    req = urllib.request.Request(f"http://127.0.0.1:{port}{path}", data=body, headers=headers)
    with HTTP.open(req, timeout=timeout) as response:
        return json.load(response)


def wait_ready(process, port, start):
    while time.perf_counter() - start < 30:
        if process.poll() is not None:
            raise StartupError(f"Service exited with code {process.returncode} before readiness")
        try:
            request(port, "/api/ocr/get_options", timeout=0.3)
            return round(1000 * (time.perf_counter() - start), 3)
        except (urllib.error.URLError, TimeoutError, ConnectionError, ValueError):
            time.sleep(0.025)
    raise StartupError(f"Service did not start on loopback port {port}")


def normalize(text):
    return "".join(character for character in unicodedata.normalize("NFKC", text) if not character.isspace())


def edit_distance(left, right):
    row = list(range(len(right) + 1))
    for index, char in enumerate(left, 1):
        previous, row = row, [index]
        for column, other in enumerate(right, 1):
            row.append(min(previous[column] + 1, row[-1] + 1, previous[column - 1] + (char != other)))
    return row[-1]


def measure(port, fixture, options):
    body = json.dumps({"base64": fixture["base64"], "options": options}).encode()
    start = time.perf_counter()
    response = request(port, "/api/ocr", body)
    latency = round(1000 * (time.perf_counter() - start), 3)
    code = response.get("code")
    if code not in (100, 101):
        raise RuntimeError(f"{fixture['name']}: OCR rejected the request: {response}")
    blocks = response["data"] if code == 100 else []
    actual = "\n".join(block["text"] for block in blocks)
    expected = normalize(fixture["expected"])
    return {"fixture": fixture["name"], "wall_ms": latency, "server_ms": round(response["time"] * 1000, 3),
            "code": code, "actual": actual, "blocks": len(blocks), "expected_chars": len(expected),
            "character_edits": edit_distance(expected, normalize(actual))}


def start_hidden(command, cwd, diagnostics):
    startup = subprocess.STARTUPINFO()
    startup.dwFlags = subprocess.STARTF_USESHOWWINDOW
    startup.wShowWindow = subprocess.SW_HIDE
    return subprocess.Popen(command, cwd=cwd, stdin=subprocess.DEVNULL,
                            stdout=diagnostics, stderr=diagnostics, startupinfo=startup,
                            creationflags=subprocess.CREATE_NO_WINDOW)


def run_service(command, cwd, port, fixtures, options, repetitions, idle_seconds, baseline_threads=None):
    with tempfile.TemporaryFile() as diagnostics:
        start = time.perf_counter()
        process = start_hidden(command, cwd, diagnostics)
        sampler = ProcessSampler(process.pid)
        try:
            ready = wait_ready(process, port, start)
            ready_snapshot = sampler.read()
            time.sleep(idle_seconds)
            idle = sampler.read()
            first = measure(port, fixtures[0], options)
            first_snapshot = sampler.read()
            if baseline_threads is not None:
                engine = next((child for child in sampler.root.children(recursive=True)
                               if child.name().lower() == "paddleocr-json.exe"), None)
                arguments = engine.cmdline() if engine is not None else []
                switches = set(zip(arguments, arguments[1:]))
                if ("--cpu_threads", str(baseline_threads)) not in switches:
                    raise RuntimeError(f"PaddleOCR did not start with --cpu_threads {baseline_threads}")
                if "--enable_mkldnn=True" not in arguments:
                    raise RuntimeError("PaddleOCR did not start with MKLDNN enabled")
            for fixture in fixtures:
                measure(port, fixture, options)
            samples = []
            warm_start = sampler.read()
            started = time.perf_counter()
            for repetition in range(repetitions):
                offset = repetition % len(fixtures)
                for fixture in fixtures[offset:] + fixtures[:offset]:
                    samples.append(measure(port, fixture, options))
            batch_seconds = time.perf_counter() - started
            warm_end = sampler.read()
            time.sleep(idle_seconds)
            loaded = sampler.read()
            sampler.stop.set()
            sampler.thread.join()
            memory_samples = [*sampler.samples, idle, loaded]
            peak = {key: max(sample[key] for sample in memory_samples) for key in ("ws_mib", "private_mib")}
            zero = {"monotonic_seconds": start, "cpu_user_seconds": 0.0, "cpu_kernel_seconds": 0.0}
            cpu = {
                "startup": cpu_interval(zero, ready_snapshot),
                "idle": cpu_interval(ready_snapshot, idle),
                "first": cpu_interval(idle, first_snapshot),
                "warm": cpu_interval(warm_start, warm_end),
                "loaded_idle": cpu_interval(warm_end, loaded),
                "warm_peak_250ms_logical_cores": warm_cpu_peak(sampler.samples, warm_start, warm_end),
            }
            return {"ready_ms": ready, "first": first, "idle": idle, "loaded": loaded, "peak": peak,
                    "cpu": cpu, "memory_sample_count": len(memory_samples), "batch_seconds": batch_seconds, "samples": samples}
        except Exception:
            diagnostics.seek(0)
            print(diagnostics.read().decode(errors="replace")[-7000:], flush=True)
            for log in (cwd / "UmiOCR-data" / "logs").glob("*.log"):
                print(log.read_text(encoding="utf-8", errors="replace")[-3000:], flush=True)
            raise
        finally:
            sampler.close()
            process.wait(timeout=5)


def desktop_window(pid):
    user = ctypes.WinDLL("user32", use_last_error=True)
    user.FindWindowExW.argtypes = [wintypes.HWND, wintypes.HWND, wintypes.LPCWSTR, wintypes.LPCWSTR]
    user.FindWindowExW.restype = wintypes.HWND
    user.GetWindowThreadProcessId.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.DWORD)]
    user.GetWindowThreadProcessId.restype = wintypes.DWORD
    window = None
    while window := user.FindWindowExW(None, window, "WfZiUiBp_Desktop", None):
        owner = wintypes.DWORD()
        user.GetWindowThreadProcessId(window, ctypes.byref(owner))
        if owner.value == pid:
            return window
    return None


def request_desktop_exit(pid):
    window = desktop_window(pid)
    if window is None:
        return False
    post = ctypes.WinDLL("user32", use_last_error=True).PostMessageW
    post.argtypes = [wintypes.HWND, wintypes.UINT, wintypes.WPARAM, wintypes.LPARAM]
    post.restype = wintypes.BOOL
    # The app's control-window protocol targets this PID only and removes its tray icon on exit.
    return bool(post(window, 0x8004, 5, 0))


def desktop_idle(exe, models, repetitions, idle_seconds):
    trials = []
    with tempfile.TemporaryDirectory(prefix="wf-zi-ui-bp-idle-") as scratch:
        root = Path(scratch)
        frozen, sidecars = freeze_executable(exe, root)
        for index in range(repetitions):
            profile = root / f"profile-{index}.json"
            profile.write_text('{"hotkey":false,"tray":true,"auto_copy":false}', encoding="utf-8")
            command = [str(frozen), "--hide", "--no-hotkey", "--profile", str(profile), "--models", str(models)]
            with tempfile.TemporaryFile() as diagnostics:
                started = time.perf_counter()
                process = start_hidden(command, root, diagnostics)
                sampler = ProcessSampler(process.pid)
                try:
                    while not desktop_window(process.pid):
                        if process.poll() is not None or time.perf_counter() - started > 20:
                            raise StartupError("Rust desktop control window did not initialize")
                        time.sleep(0.025)
                    ready_ms = (time.perf_counter() - started) * 1000
                    ready_snapshot = sampler.read()
                    time.sleep(idle_seconds)
                    idle_snapshot = sampler.read()
                    trials.append({"ready_ms": ready_ms, **idle_snapshot, "idle_cpu": cpu_interval(ready_snapshot, idle_snapshot)})
                finally:
                    if process.poll() is None and request_desktop_exit(process.pid):
                        try:
                            process.wait(timeout=2)
                        except subprocess.TimeoutExpired:
                            pass
                    sampler.close()
                    process.wait(timeout=5)
        return {
            "method": "Fresh Rust native tray via --hide with F4 disabled and isolated profile; no main window opened. Idle interval begins when the desktop command window is ready.",
            "sha256": hash_file(frozen),
            "runtime_sidecars_sha256": sidecars,
            "idle_seconds": idle_seconds,
            "median": {
                **{key: statistics.median(sample[key] for sample in trials) for key in ("ready_ms", "ws_mib", "private_mib")},
                **{key: statistics.median(sample["idle_cpu"][key] for sample in trials)
                   for key in ("cpu_total_seconds", "average_logical_cores", "average_machine_percent")},
            },
            "trials": trials,
        }


def summarize(trials):
    samples = [sample for trial in trials for sample in trial["samples"]]
    wall = sorted(sample["wall_ms"] for sample in samples)
    return {
        "ready_p50_ms": statistics.median(trial["ready_ms"] for trial in trials),
        "first_p50_ms": statistics.median(trial["first"]["wall_ms"] for trial in trials),
        "warm_p50_ms": statistics.median(wall),
        "warm_p95_ms": wall[math.ceil(0.95 * len(wall)) - 1],
        "warm_p99_ms": wall[math.ceil(0.99 * len(wall)) - 1],
        "warm_max_ms": wall[-1],
        "warm_throughput_images_s": 1000 * len(samples) / sum(wall),
        "warm_batch_loop_images_s": len(samples) / sum(trial["batch_seconds"] for trial in trials),
        "character_error_rate": sum(sample["character_edits"] for sample in samples) / max(1, sum(sample["expected_chars"] for sample in samples)),
        "memory_median_mib": {
            phase: {key: statistics.median(trial[phase][key] for trial in trials) for key in ("ws_mib", "private_mib")}
            for phase in ("idle", "loaded", "peak")
        },
        "cpu": {
            "startup_cpu_ms_p50": 1000 * statistics.median(trial["cpu"]["startup"]["cpu_total_seconds"] for trial in trials),
            "first_cpu_ms_p50": 1000 * statistics.median(trial["cpu"]["first"]["cpu_total_seconds"] for trial in trials),
            "warm_cpu_ms_per_image": 1000 * sum(trial["cpu"]["warm"]["cpu_total_seconds"] for trial in trials) / len(samples),
            "warm_user_cpu_ms_per_image": 1000 * sum(trial["cpu"]["warm"]["cpu_user_seconds"] for trial in trials) / len(samples),
            "warm_kernel_cpu_ms_per_image": 1000 * sum(trial["cpu"]["warm"]["cpu_kernel_seconds"] for trial in trials) / len(samples),
            "warm_peak_250ms_logical_cores_max": max(trial["cpu"]["warm_peak_250ms_logical_cores"] for trial in trials),
            "phases": {
                phase: {key: statistics.median(trial["cpu"][phase][key] for trial in trials)
                        for key in ("average_logical_cores", "average_machine_percent")}
                for phase in ("startup", "idle", "first", "warm", "loaded_idle")
            },
        },
        "per_fixture_p50_ms": {name: statistics.median(sample["wall_ms"] for sample in samples if sample["fixture"] == name)
                               for name in dict.fromkeys(sample["fixture"] for sample in samples)},
        "samples": len(samples),
    }


def save_results(output, metadata, fixtures, results):
    summary = {variant: summarize(trials) for variant, trials in results.items() if trials}
    output.parent.mkdir(parents=True, exist_ok=True)
    pending = output.with_suffix(output.suffix + ".tmp")
    pending.write_text(json.dumps({"metadata": metadata,
        "fixtures": [{key: value for key, value in item.items() if key != "base64"} for item in fixtures],
        "summary": summary, "trials": results}, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    pending.replace(output)
    return summary


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--umi", type=Path, help="Original Umi-OCR Paddle 2.1.5 installation directory")
    parser.add_argument("--rust", type=Path, default=PROJECT / "target" / "release" / "wf-zi-ui-bp.exe")
    parser.add_argument("--models", type=Path, default=PROJECT / "models")
    parser.add_argument("--rust-arg", action="append", default=[], help="Additional Rust argument; use --rust-arg=--flag")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--fixtures", type=Path, help="Optional output folder for generated PNGs")
    parser.add_argument("--threads", type=int, default=4)
    parser.add_argument("--trials", type=int, default=3)
    parser.add_argument("--repetitions", type=int, default=6)
    parser.add_argument("--idle-seconds", type=float, default=2.0)
    parser.add_argument("--baseline-only", action="store_true")
    parser.add_argument("--include-new-model", action="store_true", help="Also compare Rust with official PP-OCRv6 Tiny")
    parser.add_argument("--desktop-idle-only", action="store_true", help="Sample only the hidden Rust desktop (no hotkey or OCR loaded)")
    args = parser.parse_args()
    if min(args.threads, args.trials, args.repetitions) < 1:
        parser.error("threads, trials and repetitions must all be positive")
    if not math.isfinite(args.idle_seconds) or args.idle_seconds < 0.5:
        parser.error("idle-seconds must be finite and at least 0.5")
    if args.baseline_only and args.include_new_model:
        parser.error("--include-new-model requires the Rust service")
    rust = args.rust.resolve()
    if args.desktop_idle_only:
        data = desktop_idle(rust, args.models.resolve(), args.trials, args.idle_seconds)
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(data, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
        print(json.dumps(data["median"], indent=2))
        return
    if args.umi is None:
        parser.error("--umi is required for a service benchmark")
    fixtures = make_fixtures(args.fixtures)
    umi = args.umi.resolve()
    baseline_exe = umi / "Umi-OCR.exe"
    baseline_engine = umi / "UmiOCR-data" / "plugins" / "win7_x64_PaddleOCR-json" / "PaddleOCR-json.exe"
    metadata = {
        "utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "os": platform.platform(),
        "cpu": os.environ.get("PROCESSOR_IDENTIFIER"),
        "logical_cpus": os.cpu_count(),
        "threads_per_engine": args.threads,
        "trials": args.trials,
        "repetitions_per_trial": args.repetitions,
        "idle_seconds": args.idle_seconds,
        "sample_interval_seconds": SAMPLE_INTERVAL,
        "cpu_peak_window_seconds": CPU_PEAK_WINDOW,
        "options": OCR_OPTIONS,
        "startup_retries": [],
        "sha256": {"umi": hash_file(baseline_exe), "paddle_engine": hash_file(baseline_engine)},
        "method": "Fresh separate processes; Umi portable copy with native Qt, main window hidden and MKLDNN enabled; Rust HTTP mode; F4 helper excluded; no disk-cache purge; process-tree working set, private memory and user/kernel CPU sampled at roughly 25 ms.",
    }
    variants = ["umi"]
    if not args.baseline_only:
        variants.append("rust")
    if args.include_new_model:
        variants.append("rust_v6_tiny")
    options = {variant: dict(OCR_OPTIONS) for variant in variants}
    if args.include_new_model:
        options["rust_v6_tiny"]["ocr.language"] = "v6_tiny"
    metadata["variant_options"] = options
    results = {variant: [] for variant in variants}
    with tempfile.TemporaryDirectory(prefix="wf-zi-ui-bp-bench-") as scratch:
        root = Path(scratch)
        if not args.baseline_only:
            rust_copy, sidecars = freeze_executable(rust, root)
            metadata["sha256"]["rust"] = hash_file(rust_copy)
            metadata["sha256"]["runtime_sidecars"] = sidecars
            metadata["sha256"]["model_manifests"] = {
                path.name: hash_file(path) for path in args.models.glob("*-provenance.json")
            }
            active_assets = ["det.onnx", "rec_chinese.onnx", "dict_chinese.txt", "onnxruntime.dll"]
            if (args.models / "onnxruntime_providers_shared.dll").is_file():
                active_assets.append("onnxruntime_providers_shared.dll")
            if args.include_new_model:
                active_assets.extend(["det_v6_tiny.onnx", "rec_v6_tiny.onnx", "dict_v6_tiny.txt"])
            metadata["sha256"]["active_model_assets"] = {name: hash_file(args.models / name) for name in active_assets}
        for trial in range(args.trials):
            offset = trial % len(variants)
            order = variants[offset:] + variants[:offset]
            for variant in order:
                for attempt in range(1, 4):
                    port = reserve_port()
                    if variant == "umi":
                        cwd = root / f"umi-{trial}-{attempt}"
                        stage_umi(umi, cwd, port, args.threads)
                        command = [str(cwd / "Umi-OCR.exe"), "--force"]
                    else:
                        cwd = root
                        command = [str(rust_copy), "--serve", "--port", str(port), "--no-hotkey", "--threads", str(args.threads),
                                   "--models", str(args.models.resolve()), *args.rust_arg]
                    print(f"{variant}: trial {trial + 1}/{args.trials}, attempt {attempt}", flush=True)
                    try:
                        result = run_service(command, cwd, port, fixtures, options[variant], args.repetitions, args.idle_seconds,
                                             args.threads if variant == "umi" else None)
                    except StartupError as error:
                        metadata["startup_retries"].append({"variant": variant, "trial": trial + 1, "attempt": attempt, "error": str(error)})
                        save_results(args.output, metadata, fixtures, results)
                        if attempt == 3:
                            raise
                    else:
                        results[variant].append(result)
                        save_results(args.output, metadata, fixtures, results)
                        break
    summary = save_results(args.output, metadata, fixtures, results)
    print(json.dumps(summary, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
