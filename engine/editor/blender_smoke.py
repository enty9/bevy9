"""Linux desktop integration smoke. Requires GTK 3 (PyGObject), python-xlib and xdotool."""
from contextlib import closing
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
from Xlib import X, Xutil, display
from Xlib.protocol import event

gi.require_version('Gtk', '3.0')
gi.require_version('GdkX11', '3.0')
from gi.repository import Gtk, GdkX11  # registers get_xid


# Observe a click inside Blender, not just the dimensions reported by X11.
CLICK_PROBE = """
import bpy, json, sys
from pathlib import Path
from bpy.types import Operator

class ClickProbe(Operator):
    bl_idname = 'wm.bevy9_click_probe'
    bl_label = 'Bevy9 smoke test'

    def execute(self, context):
        context.window_manager.modal_handler_add(self)
        return {'RUNNING_MODAL'}

    def modal(self, context, event):
        if event.type == 'LEFTMOUSE' and event.value == 'PRESS':
            path = Path(sys.argv[-1]) / 'click.json'
            size = [context.window.width, context.window.height]
            path.with_suffix('.tmp').write_text(json.dumps([size, [event.mouse_x, event.mouse_y]]))
            path.with_suffix('.tmp').replace(path)
            return {'RUNNING_MODAL'}
        return {'PASS_THROUGH'}

bpy.utils.register_class(ClickProbe)
bpy.ops.wm.bevy9_click_probe()
"""


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
    blender = [env.get('BEVY9_BLENDER') or 'blender', '--factory-startup', '--disable-autoexec',
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
            '-p', '0', '0', '800', '600',
            '-P', driver, '--python-expr',
            "import bpy; bpy.context.active_object.modifiers.new('Smoke bevel', 'BEVEL')",
            '--python-expr', CLICK_PROBE,
            '--', 'edit', root], env=env)
        try:
            status = root / 'status.json'
            wait(lambda: status.exists() and json.loads(status.read_text())['ready'], child)
            def visible():
                found = subprocess.run(['xdotool', 'search', '--onlyvisible', '--all', '--pid',
                    str(child.pid), '--class', '^blender$'], capture_output=True, text=True)
                return found.stdout.strip()
            win = wait(visible, child)
            with closing(display.Display()) as conn:
                window = conn.create_resource_object('window', int(win))
                wait(lambda: window.query_tree().parent.id == xid, child)

                def check_click(rect):
                    result = root / 'click.json'
                    result.unlink(missing_ok=True)
                    x, y = rect['width'] // 3, rect['height'] // 4
                    screen = conn.screen().root
                    pos = screen.translate_coords(xid, rect['x'] + x, rect['y'] + y)
                    # Deliver native input to the test window without warping the desktop pointer.
                    point = dict(root=screen, window=window, child=X.NONE, time=X.CurrentTime,
                                 root_x=pos.x, root_y=pos.y, event_x=x, event_y=y, same_screen=True)
                    window.send_event(event.MotionNotify(detail=0, state=0, **point))
                    window.send_event(event.ButtonPress(detail=1, state=0, **point))
                    window.send_event(event.ButtonRelease(detail=1, state=X.Button1Mask, **point))
                    conn.flush()
                    wait(result.exists, child)
                    observed = json.loads(result.read_text())
                    # Blender reports mouse coordinates from the bottom left.
                    assert observed == [[rect['width'], rect['height']], [x, rect['height'] - 1 - y]], observed

                check_click(rect)
                (root / 'layout.json').write_text('null')
                wait(lambda: not visible(), child)
                # Reproduce a WM leaving the child withdrawn, even on a bare X server.
                state = conn.intern_atom('WM_STATE')
                window.change_property(state, state, 32, [Xutil.WithdrawnState, X.NONE])
                conn.sync()
                rect = dict(x=75, y=18, width=520, height=410)
                (root / 'layout.json').write_text(json.dumps(rect))
                wait(visible, child)
                check_click(rect)
            (root / 'save').write_text('smoke')
            wait(lambda: json.loads(status.read_text()).get('reply') == 'smoke', child)
            saved_state = json.loads(status.read_text())
            assert not saved_state['error'], saved_state
            assert saved_state['saved'] >= 1 and not saved_state['dirty'], saved_state
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
    print('PASS: embedded Blender, verified mouse coordinates before/after tab switch and resize, saved GLB, reopened editable .blend')


if __name__ == '__main__':
    main()
