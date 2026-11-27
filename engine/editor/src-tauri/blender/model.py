# SPDX-License-Identifier: GPL-3.0-or-later
"""Executed by Blender, on Blender's main thread. No network listener."""
import json
import os
from pathlib import Path
import sys
import tempfile

import bpy


def export_glb(path):
    result = bpy.ops.export_scene.gltf(filepath=str(path), export_format='GLB', export_apply=True,
                                       export_cameras=False, export_lights=False)
    if 'FINISHED' not in result:
        raise RuntimeError('Model export was cancelled')


def write_json(path, data):
    tmp = path.with_suffix('.tmp')
    tmp.write_text(json.dumps(data), encoding='utf-8')
    os.replace(tmp, path)


def start(work):
    config = json.loads((work / 'session.json').read_text(encoding='utf-8'))
    asset, source = (Path(config[k]) for k in ('asset', 'source'))
    marker = Path(str(asset) + '.b9meta')
    source_root = source.parent.resolve(strict=True)
    asset_root = Path(config['assets_root'])
    original = asset.stat()
    prefs = bpy.context.preferences
    prefs.view.show_splash = False
    prefs.filepaths.save_version = 0
    if not source.exists():
        bpy.ops.object.select_all(action='SELECT')
        bpy.ops.object.delete(use_global=False)
        if 'FINISHED' not in bpy.ops.import_scene.gltf(filepath=str(asset)):
            raise RuntimeError('Could not import model')
    # Ctrl+S writes to a session document, never to another project's file.
    working = work / 'model.blend'
    bpy.ops.wm.save_as_mainfile(filepath=str(working), relative_remap=True)

    state = {'ready': False, 'dirty': False, 'error': None, 'saved': 0, 'reply': None}
    write_status = lambda: write_json(work / 'status.json', state)

    def publish():
        nonlocal original
        if (asset.resolve(strict=True) != asset or not asset.is_relative_to(asset_root)
                or source.parent.resolve(strict=True) != source_root or source.is_symlink()
                or config['uuid'] not in marker.read_text(encoding='utf-8')):
            raise RuntimeError('Asset was moved or replaced; refusing to overwrite')
        current = asset.stat()
        if (current.st_ino, current.st_mtime_ns, current.st_size) != (
                original.st_ino, original.st_mtime_ns, original.st_size):
            raise RuntimeError('The GLB changed outside this tab; refusing to overwrite')
        # Stage both outputs on the destination filesystems. The asset watcher
        # sees only complete GLBs, and the existing UUID sidecar is untouched.
        with tempfile.TemporaryDirectory(dir=asset.parent, prefix='.model-') as a:
            glb = Path(a) / 'model.glb'
            export_glb(glb)
            bpy.ops.file.pack_all()
            # Blender itself stages a .blend before replacing the existing file.
            # Remap relative library/texture paths to the real source directory.
            if 'FINISHED' not in bpy.ops.wm.save_as_mainfile(filepath=str(source), copy=True,
                                                            relative_remap=True):
                raise RuntimeError('Could not save Blender document')
            os.replace(glb, asset)
        original = asset.stat()
        state.update(error=None, dirty=False, saved=state['saved'] + 1)

    publishing = False

    def after_save(filepath, *_):
        nonlocal publishing
        if publishing or Path(filepath) != working:
            return
        publishing = True
        try:
            publish()
        except Exception as exc:
            state.update(error=str(exc), dirty=True)
        finally:
            publishing = False
            write_status()

    bpy.app.handlers.save_post.append(after_save)
    # Load only the editor's bundled adapter, not Python from the project.
    from runpy import run_path
    attach = run_path(str(work / 'embed.py'))['attach']
    place = attach(int(config['parent']))
    last_layout = None
    state['ready'] = True
    write_status()

    def tick():
        nonlocal last_layout
        try:
            layout_file = work / 'layout.json'
            layout = layout_file.read_bytes() if layout_file.exists() else b'null'
            if layout != last_layout:
                place(json.loads(layout))
                last_layout = layout
            request = work / 'save'
            if request.exists():
                token = request.read_text()
                request.unlink()
                state['reply'] = token
                if Path(bpy.data.filepath) != working:
                    raise RuntimeError('The active Blender file is not the model document')
                # The save handler exports the model and reports errors.
                if 'FINISHED' not in bpy.ops.wm.save_as_mainfile(filepath=str(working)):
                    raise RuntimeError('Blender cancelled save')
            if bpy.data.is_dirty and not state['dirty']:
                state['dirty'] = True
                write_status()
        except Exception as exc:
            if state['error'] != str(exc):
                state['error'] = str(exc)
            write_status()
        return 0.1

    bpy.app.timers.register(tick, persistent=True)


if __name__ == '__main__':
    mode, arg = sys.argv[sys.argv.index('--') + 1:]
    if mode == 'create':
        for obj in list(bpy.context.scene.objects):
            if obj.type != 'MESH':
                bpy.data.objects.remove(obj, do_unlink=True)
        export_glb(Path(arg))
    elif mode == 'edit':
        start(Path(arg))
