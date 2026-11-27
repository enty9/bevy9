"""Linux desktop integration smoke: a real Blender window inside a disposable GTK host."""
import json
import os
from pathlib import Path
import shutil
import struct
import subprocess
import tempfile
import time
import uuid

import gi

gi.require_version('Gtk', '3.0')
gi.require_version('GdkX11', '3.0')
from gi.repository import Gtk, GdkX11  # registers get_xid


def wait(fn, child):
    end = time.monotonic() + 45
    while time.monotonic() < end:
        while Gtk.events_pending():
            Gtk.main_iteration()
        if child.poll() is not None:
            raise RuntimeError(f'Blender exited with {child.returncode}')
        value = fn()
        if value:
            return value
        time.sleep(0.05)
    raise TimeoutError('Blender did not acknowledge the request')


def main():
    host = Gtk.Window(title='Bevy9 Blender integration test')
    host.set_default_size(720, 480)
    host.show_all()
    xid = host.get_window().get_xid()
    driver = Path(__file__).parent / 'src-tauri/blender/model.py'
    env = dict(os.environ, XDG_SESSION_TYPE='x11')
    env.pop('WAYLAND_DISPLAY', None)
    env.pop('WAYLAND_SOCKET', None)
    blender = [env.get('BEVY9_BLENDER', 'blender'), '--factory-startup', '--disable-autoexec',
               '-noaudio', '--python-exit-code', '1']
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        asset = root / 'Test model.glb'
        source = root / 'source.blend'
        uid = str(uuid.uuid4())
        subprocess.run([*blender, '-b', '-P', driver, '--', 'create', asset], check=True, env=env)
        original = asset.read_bytes()
        marker = Path(str(asset) + '.b9meta')
        marker.write_text(f'(uuid: ("{uid}"))')
        (root / 'session.json').write_text(json.dumps(dict(parent=xid, source=str(source),
            asset=str(asset), assets_root=str(root), uuid=uid)))
        shutil.copy(driver.with_name('embed.py'), root)
        rect = dict(x=20, y=45, width=600, height=360)
        (root / 'layout.json').write_text(json.dumps(rect))
        child = subprocess.Popen([*blender, '--no-window-frame', '--no-window-focus', '-w',
            '-P', driver, '--python-expr',
            "import bpy; bpy.context.active_object.modifiers.new('Smoke bevel', 'BEVEL')",
            '--', 'edit', root], env=env)
        try:
            status = root / 'status.json'
            wait(lambda: status.exists() and json.loads(status.read_text())['ready'], child)
            def visible():
                found = subprocess.run(['xdotool', 'search', '--onlyvisible', '--all', '--pid',
                    str(child.pid), '--class', '^blender$'], capture_output=True, text=True)
                return found.stdout.strip()
            win = wait(visible, child)
            from ctypes import CDLL, byref, c_uint, c_ulong, c_void_p
            x11 = CDLL('libX11.so.6')
            x11.XOpenDisplay.restype = c_void_p
            display = c_void_p(x11.XOpenDisplay(None))
            def parented():
                parent, screen, children, count = c_ulong(), c_ulong(), c_void_p(), c_uint()
                x11.XQueryTree(display, c_ulong(int(win)), byref(screen), byref(parent), byref(children), byref(count))
                if children: x11.XFree(children)
                return parent.value == xid
            wait(parented, child)
            (root / 'layout.json').write_text('null')
            wait(lambda: not visible(), child)
            (root / 'layout.json').write_text(json.dumps(rect))
            wait(visible, child)
            (root / 'save').write_text('smoke')
            wait(lambda: json.loads(status.read_text()).get('reply') == 'smoke', child)
            assert not json.loads(status.read_text())['error'], status.read_text()
            assert source.is_file() and marker.read_text() == f'(uuid: ("{uid}"))'
            data = asset.read_bytes()
            assert data[:4] == b'glTF' and data != original
            gltf = json.loads(data[20:20 + struct.unpack_from('<I', data, 12)[0]])
            assert gltf['meshes'] and gltf['scenes']
        finally:
            child.kill()
            child.wait()
        subprocess.run([*blender, '-b', source, '--python-expr',
            "import bpy; assert any('Smoke bevel' in o.modifiers for o in bpy.data.objects)"],
            check=True, env=env)
    print('PASS: embedded Blender, switched tab, saved GLB, reopened editable .blend')


if __name__ == '__main__':
    main()
