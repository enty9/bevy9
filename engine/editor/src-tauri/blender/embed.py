# SPDX-License-Identifier: GPL-3.0-or-later
"""Parent this Blender process's own main window into the editor's model tab."""
import os
import sys
import time


def clear_wm_state(window):
    """Discard the former top-level's ICCCM state before showing it as a child."""
    import ctypes as c

    x11 = c.CDLL('libX11.so.6')
    x11.XOpenDisplay.argtypes = [c.c_char_p]
    x11.XOpenDisplay.restype = c.c_void_p
    x11.XInternAtom.argtypes = [c.c_void_p, c.c_char_p, c.c_int]
    x11.XInternAtom.restype = c.c_ulong
    x11.XDeleteProperty.argtypes = [c.c_void_p, c.c_ulong, c.c_ulong]
    x11.XCloseDisplay.argtypes = [c.c_void_p]

    display = x11.XOpenDisplay(None)
    if not display:
        raise RuntimeError('Could not open the Blender X11 display')
    try:
        atom = x11.XInternAtom(display, b'WM_STATE', True)
        if atom:
            x11.XDeleteProperty(display, int(window), atom)
    finally:
        x11.XCloseDisplay(display)


def attach(parent):
    if sys.platform == 'win32':
        return attach_windows(parent)
    import subprocess

    def xdo(*args):
        return subprocess.check_output(['xdotool', *map(str, args)], text=True).strip()

    deadline = time.monotonic() + 5.0
    window = None
    last_err = None
    while time.monotonic() < deadline:
        try:
            found = xdo('search', '--all', '--onlyvisible', '--pid', os.getpid(), '--class', '^blender$').splitlines()
            if found:
                window = found[0]
                break
        except subprocess.CalledProcessError as exc:
            last_err = exc
        time.sleep(0.05)
    if not window:
        raise RuntimeError(f'Blender main window not found ({last_err})')

    xdo('windowunmap', window, 'windowreparent', window, parent)
    visible = False

    def place(rect):
        nonlocal visible
        if rect:
            if not visible:
                # Some WMs (e.g. xwayland-satellite) leave WithdrawnState on
                # reparent. Blender treats that as minimized and skips resize
                # events, including updating the height used to invert mouse Y.
                clear_wm_state(window)
            xdo('windowmove', window, rect['x'], rect['y'], 'windowsize', window,
                rect['width'], rect['height'], 'windowmap', window, 'windowraise', window)
            if not visible:
                subprocess.run(
                    ['xdotool', 'windowfocus', window],
                    check=False,
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL,
                )
        elif visible:
            xdo('windowunmap', window)
        visible = bool(rect)

    return place


def attach_windows(parent):
    import ctypes as c
    from ctypes import wintypes as w

    user = c.windll.user32
    callback = c.WINFUNCTYPE(w.BOOL, w.HWND, w.LPARAM)
    user.GetWindowThreadProcessId.argtypes = [w.HWND, c.POINTER(w.DWORD)]
    user.IsWindowVisible.argtypes = [w.HWND]
    user.GetClassNameW.argtypes = [w.HWND, w.LPWSTR, c.c_int]
    user.EnumWindows.argtypes = [callback, w.LPARAM]
    user.SetParent.argtypes = [w.HWND, w.HWND]
    user.SetParent.restype = w.HWND
    user.GetParent.argtypes = [w.HWND]
    user.GetParent.restype = w.HWND
    user.GetWindowLongW.argtypes = [w.HWND, c.c_int]
    user.SetWindowLongW.argtypes = [w.HWND, c.c_int, w.LONG]
    user.SetWindowPos.argtypes = [w.HWND, w.HWND, c.c_int, c.c_int, c.c_int, c.c_int, w.UINT]
    user.ShowWindow.argtypes = [w.HWND, c.c_int]

    windows = []

    @callback
    def visit(hwnd, _):
        pid = w.DWORD()
        user.GetWindowThreadProcessId(hwnd, c.byref(pid))
        name = c.create_unicode_buffer(256)
        user.GetClassNameW(hwnd, name, len(name))
        if pid.value == os.getpid() and user.IsWindowVisible(hwnd) and name.value == 'GHOST_WindowClass':
            windows.append(hwnd)
        return True

    deadline = time.monotonic() + 5.0
    while time.monotonic() < deadline and not windows:
        user.EnumWindows(visit, 0)
        if not windows:
            time.sleep(0.05)
    if not windows:
        raise RuntimeError('Blender main window not found')

    hwnd = windows[0]
    parent_hwnd = w.HWND(parent)
    user.ShowWindow(hwnd, 0)
    style = user.GetWindowLongW(hwnd, -16)
    user.SetWindowLongW(hwnd, -16, (style & ~0x80CF0000) | 0x40000000)  # WS_CHILD
    user.SetParent(hwnd, parent_hwnd)
    if (user.GetParent(hwnd) or 0) != parent:
        raise RuntimeError('Windows refused to embed the Blender window')

    def place(rect):
        if rect:
            if not user.SetWindowPos(hwnd, 0, rect['x'], rect['y'], rect['width'], rect['height'], 0x60):
                raise RuntimeError('Could not position the Blender window')
        else:
            user.ShowWindow(hwnd, 0)

    return place
