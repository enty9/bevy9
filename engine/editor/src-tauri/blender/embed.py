# SPDX-License-Identifier: GPL-3.0-or-later
"""Parent this Blender process's own main window into the editor's model tab."""
import os
import sys


def attach(parent):
    if sys.platform == 'win32':
        return attach_windows(parent)
    import subprocess

    def xdo(*args):
        return subprocess.check_output(['xdotool', *map(str, args)], text=True).strip()

    window = xdo('search', '--all', '--onlyvisible', '--pid', os.getpid(), '--class', '^blender$').splitlines()[0]
    xdo('windowunmap', window, 'windowreparent', window, parent)
    visible = False

    def place(rect):
        nonlocal visible
        if rect:
            args = ['windowmove', window, rect['x'], rect['y'], 'windowsize', window,
                    rect['width'], rect['height'], 'windowmap', window, 'windowraise', window]
            if not visible:
                args.extend(['windowfocus', window])
            xdo(*args)
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
    user.SetParent.argtypes = [w.HWND, w.HWND]
    user.SetParent.restype = w.HWND
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
        user.GetClassNameW(w.HWND(hwnd), name, len(name))
        if pid.value == os.getpid() and user.IsWindowVisible(hwnd) and name.value == 'GHOST_WindowClass':
            windows.append(hwnd)
        return True

    user.EnumWindows(visit, 0)
    if not windows:
        raise RuntimeError('Blender main window not found')
    hwnd = windows[0]
    user.ShowWindow(hwnd, 0)
    style = user.GetWindowLongW(hwnd, -16)
    user.SetWindowLongW(hwnd, -16, (style & ~0x80CF0000) | 0x40000000)  # WS_CHILD
    user.SetParent(hwnd, parent)
    user.GetParent.argtypes = [w.HWND]
    user.GetParent.restype = w.HWND
    if user.GetParent(hwnd) != parent:
        raise RuntimeError('Windows refused to embed the Blender window')

    def place(rect):
        if rect:
            if not user.SetWindowPos(hwnd, 0, rect['x'], rect['y'], rect['width'], rect['height'], 0x60):
                raise RuntimeError('Could not position the Blender window')
        else:
            user.ShowWindow(hwnd, 0)

    return place
