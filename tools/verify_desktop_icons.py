"""Verify the packaged icon and optionally inspect an already-running app."""

import argparse
import ctypes as ct
from ctypes import wintypes as wt
import hashlib
import json
from pathlib import Path
import struct


ROOT = Path(__file__).resolve().parents[1]
ICON_SHA256 = "5efa713be8e51cd1367d032f676e195697a99ffd4e5e6711617a77490adc9703"
CONTROL_CLASS = "WfZiUiBp_Desktop"
ICON_ID = 1
TRAY_ID = 1


def winapi(dll, name, arguments, result):
    function = getattr(dll, name)
    function.argtypes = arguments
    function.restype = result
    return function


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def verify_pe_icon(executable):
    kernel = ct.WinDLL("kernel32", use_last_error=True)
    load = winapi(kernel, "LoadLibraryExW", [wt.LPCWSTR, wt.HANDLE, wt.DWORD], wt.HMODULE)
    free = winapi(kernel, "FreeLibrary", [wt.HMODULE], wt.BOOL)
    find = winapi(kernel, "FindResourceW", [wt.HMODULE, ct.c_void_p, ct.c_void_p], wt.HRSRC)
    size = winapi(kernel, "SizeofResource", [wt.HMODULE, wt.HRSRC], wt.DWORD)
    load_resource = winapi(kernel, "LoadResource", [wt.HMODULE, wt.HRSRC], wt.HGLOBAL)
    lock = winapi(kernel, "LockResource", [wt.HGLOBAL], ct.c_void_p)

    source = (ROOT / "assets" / "umiocr.ico").read_bytes()
    if hashlib.sha256(source).hexdigest() != ICON_SHA256:
        raise RuntimeError("The project ICO does not match the recorded official digest.")

    # Data-file and image-resource mode maps the file without executing it.
    module = load(str(executable), None, 0x22)
    if not module:
        raise ct.WinError(ct.get_last_error())

    def resource(name, kind):
        handle = find(module, name, kind)
        if not handle:
            raise RuntimeError(f"PE resource missing: type {kind}, ID {name}.")
        address = lock(load_resource(module, handle))
        if not address:
            raise RuntimeError(f"Cannot read PE resource: type {kind}, ID {name}.")
        return ct.string_at(address, size(module, handle))

    try:
        group = resource(ICON_ID, 14)
        count = struct.unpack_from("<H", source, 4)[0]
        if group[:6] != source[:6] or len(group) != 6 + 14 * count:
            raise RuntimeError("The executable icon group does not match the ICO directory.")
        frames = []
        for index in range(count):
            entry = source[6 + 16 * index : 22 + 16 * index]
            group_entry = group[6 + 14 * index : 20 + 14 * index]
            if group_entry[:12] != entry[:12]:
                raise RuntimeError(f"Icon frame {index} metadata differs.")
            length, offset = struct.unpack_from("<II", entry, 8)
            frame_id = struct.unpack_from("<H", group_entry, 12)[0]
            frame = resource(frame_id, 3)
            if frame != source[offset : offset + length]:
                raise RuntimeError(f"Icon frame {frame_id} pixels differ.")
            frames.append({
                "width": entry[0] or 256,
                "height": entry[1] or 256,
                "sha256": hashlib.sha256(frame).hexdigest(),
            })
        return frames
    finally:
        free(module)


def process_path(pid):
    kernel = ct.WinDLL("kernel32", use_last_error=True)
    open_process = winapi(kernel, "OpenProcess", [wt.DWORD, wt.BOOL, wt.DWORD], wt.HANDLE)
    close_handle = winapi(kernel, "CloseHandle", [wt.HANDLE], wt.BOOL)
    query = winapi(
        kernel,
        "QueryFullProcessImageNameW",
        [wt.HANDLE, wt.DWORD, wt.LPWSTR, ct.POINTER(wt.DWORD)],
        wt.BOOL,
    )
    process = open_process(0x1000, False, pid)  # PROCESS_QUERY_LIMITED_INFORMATION
    if not process:
        raise ct.WinError(ct.get_last_error())
    try:
        path = ct.create_unicode_buffer(32768)
        length = wt.DWORD(len(path))
        if not query(process, 0, path, ct.byref(length)):
            raise ct.WinError(ct.get_last_error())
        return Path(path.value)
    finally:
        close_handle(process)


class TrayIdentifier(ct.Structure):
    _fields_ = [
        ("cbSize", wt.DWORD),
        ("hwnd", wt.HWND),
        ("uID", wt.UINT),
        ("guidItem", ct.c_byte * 16),
    ]


def verify_live_icons(pid, expected_digest):
    path = process_path(pid)
    if digest(path) != expected_digest:
        raise RuntimeError(f"PID {pid} is running a different executable: {path}")

    user = ct.WinDLL("user32", use_last_error=True)
    shell = ct.WinDLL("shell32", use_last_error=True)
    set_dpi = winapi(user, "SetThreadDpiAwarenessContext", [ct.c_void_p], ct.c_void_p)
    set_dpi(ct.c_void_p(-4))  # Per-monitor-aware v2, for physical screen coordinates.
    owner = winapi(user, "GetWindowThreadProcessId", [wt.HWND, ct.POINTER(wt.DWORD)], wt.DWORD)
    get_class = winapi(user, "GetClassNameW", [wt.HWND, wt.LPWSTR, ct.c_int], ct.c_int)
    get_title = winapi(user, "GetWindowTextW", [wt.HWND, wt.LPWSTR, ct.c_int], ct.c_int)
    callback_type = ct.WINFUNCTYPE(wt.BOOL, wt.HWND, wt.LPARAM)
    enum_windows = winapi(user, "EnumWindows", [callback_type, wt.LPARAM], wt.BOOL)
    icon_rect = winapi(
        shell, "Shell_NotifyIconGetRect", [ct.POINTER(TrayIdentifier), ct.POINTER(wt.RECT)], ct.c_long
    )
    get_icon = winapi(
        user,
        "SendMessageTimeoutW",
        [wt.HWND, wt.UINT, wt.WPARAM, wt.LPARAM, wt.UINT, wt.UINT, ct.POINTER(ct.c_size_t)],
        wt.LPARAM,
    )
    control_windows = []
    main_windows = []

    @callback_type
    def inspect_window(window, _data):
        window_pid = wt.DWORD()
        owner(window, ct.byref(window_pid))
        if window_pid.value == pid:
            name = ct.create_unicode_buffer(256)
            get_class(window, name, len(name))
            if name.value == CONTROL_CLASS:
                control_windows.append(window)
            else:
                get_title(window, name, len(name))
                if "WfZiUiBp" in name.value:
                    main_windows.append(window)
        return True

    if not enum_windows(inspect_window, 0):
        raise ct.WinError(ct.get_last_error())
    if len(control_windows) != 1:
        raise RuntimeError(f"Expected one tray control window for PID {pid}.")
    if len(main_windows) > 1:
        raise RuntimeError(f"Multiple main windows found for PID {pid}.")

    identity = TrayIdentifier(ct.sizeof(TrayIdentifier), control_windows[0], TRAY_ID)
    bounds = wt.RECT()
    status = icon_rect(ct.byref(identity), ct.byref(bounds))
    if status != 0 or bounds.right <= bounds.left or bounds.bottom <= bounds.top:
        raise RuntimeError(f"Shell has no tray bounds for PID {pid}, HRESULT={status & 0xFFFFFFFF:#x}.")

    window_icons = None
    if main_windows:
        window_icons = {}
        for size, label in [(0, "small"), (1, "large")]:
            value = ct.c_size_t()
            # WM_GETICON reads the current handle. It does not set icon or window state.
            if not get_icon(main_windows[0], 0x7F, size, 0, 2, 1000, ct.byref(value)) or not value.value:
                raise RuntimeError(f"Main window returned no {label} HICON.")
            window_icons[label] = hex(value.value)
    return {
        "pid": pid,
        "running_exe": str(path),
        "shell_tray_bounds": [bounds.left, bounds.top, bounds.right, bounds.bottom],
        "main_window_icon_handles": window_icons,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--exe", type=Path, default=ROOT / "dist" / "WfZiUiBp" / "WfZiUiBp.exe")
    parser.add_argument("--pid", type=int, help="inspect this existing process without starting it")
    arguments = parser.parse_args()
    try:
        executable = arguments.exe.resolve(strict=True)
        executable_sha = digest(executable)
        result = {
            "exe": str(executable),
            "exe_sha256": executable_sha,
            "official_ico_sha256": ICON_SHA256,
            "exact_icon_frames": verify_pe_icon(executable),
        }
        if arguments.pid is not None:
            result["live"] = verify_live_icons(arguments.pid, executable_sha)
    except (OSError, RuntimeError, struct.error) as error:
        parser.exit(1, f"Verification failed: {error}\n")
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
