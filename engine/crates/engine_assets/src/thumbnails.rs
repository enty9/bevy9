//! Asynchronous background 128×128 PNG thumbnail generator and disk/memory cache.

use std::{
    collections::HashMap,
    fs,
    io::Cursor,
    path::{Path, PathBuf},
    thread,
};

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine};
use bevy_ecs::{
    resource::Resource,
    system::{Res, ResMut},
};
use bevy_math::Vec3;
use crossbeam_channel::{unbounded, Receiver, Sender};
use engine_core::AssetUuid;
use engine_protocol::{AssetKind, AssetRecord};
use gltf::mesh::util::ReadIndices;
use image::{imageops::FilterType, DynamicImage, ImageFormat, Rgba, RgbaImage};

use crate::{
    ddc::DdcConfig,
    registry::{AssetRegistry, SharedAssetRegistry},
    tilemap::load_tilemap_from_file,
};

/// Edge dimension (width & height in pixels) of generated Content Browser PNG thumbnails.
pub const THUMBNAIL_SIZE: u32 = 128;

#[derive(Debug, Clone)]
struct ThumbnailJob {
    uuid: AssetUuid,
    abs_path: PathBuf,
    kind: AssetKind,
    source_hash: String,
    png_path: PathBuf,
    shared_registry: Option<SharedAssetRegistry>,
}

#[derive(Debug, Clone)]
struct ThumbnailReady {
    uuid: AssetUuid,
    source_hash: String,
    png_bytes: Vec<u8>,
}

/// ECS resource managing background PNG thumbnail generation under `.cache/ddc/thumbnails`.
#[derive(Resource)]
pub struct ThumbnailService {
    thumbnails_dir: PathBuf,
    job_tx: Sender<ThumbnailJob>,
    ready_rx: Receiver<ThumbnailReady>,
    requested_hashes: HashMap<AssetUuid, String>,
    cached_data_urls: HashMap<AssetUuid, (String, String)>,
    shared_registry: Option<SharedAssetRegistry>,
}

impl ThumbnailService {
    /// Starts a background thumbnail worker saving cached PNGs under `thumbnails_dir`.
    #[must_use]
    pub fn new(thumbnails_dir: PathBuf, shared_registry: Option<SharedAssetRegistry>) -> Self {
        let _ = fs::create_dir_all(&thumbnails_dir);
        let (job_tx, job_rx) = unbounded::<ThumbnailJob>();
        let (ready_tx, ready_rx) = unbounded::<ThumbnailReady>();

        if let Err(err) = thread::Builder::new()
            .name("engine-thumbnails".to_owned())
            .spawn(move || run_thumbnail_worker(job_rx, ready_tx))
        {
            tracing::warn!(error = %err, "failed to spawn background thumbnail worker thread");
        }

        Self {
            thumbnails_dir,
            job_tx,
            ready_rx,
            requested_hashes: HashMap::new(),
            cached_data_urls: HashMap::new(),
            shared_registry,
        }
    }

    /// Computes the deterministic `.png` disk cache path for `(uuid, source_hash)`.
    #[must_use]
    pub fn thumbnail_file_path(&self, uuid: AssetUuid, source_hash: &str) -> PathBuf {
        let prefix_len = source_hash.len().min(16);
        let hash_prefix = if prefix_len == 0 {
            "nohash"
        } else {
            &source_hash[..prefix_len]
        };
        self.thumbnails_dir
            .join(format!("{uuid}_{hash_prefix}.png"))
    }

    /// Queues asynchronous thumbnail generation for `record` if its current `source_hash` has not yet
    /// been requested or cached.
    pub fn request_for_record(&mut self, record: &AssetRecord, abs_path: PathBuf) {
        if self
            .requested_hashes
            .get(&record.uuid)
            .is_some_and(|h| h == &record.source_hash)
        {
            return;
        }
        self.requested_hashes
            .insert(record.uuid, record.source_hash.clone());
        let png_path = self.thumbnail_file_path(record.uuid, &record.source_hash);
        let _ = self.job_tx.send(ThumbnailJob {
            uuid: record.uuid,
            abs_path,
            kind: record.kind,
            source_hash: record.source_hash.clone(),
            png_path,
            shared_registry: self.shared_registry.clone(),
        });
    }

    /// Queues thumbnail generation for every indexed asset in `registry` missing a current hash thumbnail.
    pub fn request_all_missing(&mut self, registry: &AssetRegistry) {
        let snapshot = registry.snapshot(&engine_protocol::AssetListParams::default());
        let shared = registry.shared();
        for rec in snapshot.assets {
            if let Some(abs_path) = shared.resolve_abs_path(rec.uuid) {
                self.request_for_record(&rec, abs_path);
            }
        }
    }

    /// Returns a `"data:image/png;base64,..."` URI for `uuid` matching `source_hash`, loading from disk
    /// cache if already written by a previous run.
    pub fn get_data_url(&mut self, uuid: AssetUuid, source_hash: &str) -> Option<String> {
        if let Some((cached_hash, data_url)) = self.cached_data_urls.get(&uuid)
            && cached_hash == source_hash
        {
            return Some(data_url.clone());
        }

        let png_path = self.thumbnail_file_path(uuid, source_hash);
        let png_bytes = fs::read(&png_path).ok()?;
        let data_url = encode_png_data_url(&png_bytes);
        self.cached_data_urls
            .insert(uuid, (source_hash.to_owned(), data_url.clone()));
        Some(data_url)
    }

    /// Drains finished worker thumbnails, updates `registry` readiness flags, and returns the count ready.
    pub fn poll_completed(&mut self, registry: &mut AssetRegistry) -> usize {
        let mut count = 0usize;
        while let Ok(ready) = self.ready_rx.try_recv() {
            let current_hash = self
                .requested_hashes
                .get(&ready.uuid)
                .map(String::as_str);
            if current_hash != Some(ready.source_hash.as_str()) {
                continue;
            }
            let data_url = encode_png_data_url(&ready.png_bytes);
            self.cached_data_urls
                .insert(ready.uuid, (ready.source_hash, data_url));
            registry.shared().set_has_thumbnail(ready.uuid, true);
            count += 1;
        }
        if count > 0 {
            registry.revision = registry.revision.saturating_add(1);
            registry.dirty = true;
        }
        count
    }
}

/// ECS system queuing newly discovered assets and absorbing finished background PNG thumbnails.
pub fn poll_thumbnails_system(
    mut service: Option<ResMut<ThumbnailService>>,
    mut registry: Option<ResMut<AssetRegistry>>,
    ddc: Option<Res<DdcConfig>>,
) {
    let (Some(service), Some(registry)) = (service.as_deref_mut(), registry.as_deref_mut()) else {
        return;
    };
    let _ = ddc;
    service.request_all_missing(registry);
    service.poll_completed(registry);
}

fn encode_png_data_url(png_bytes: &[u8]) -> String {
    format!("data:image/png;base64,{}", BASE64_STANDARD.encode(png_bytes))
}

fn run_thumbnail_worker(job_rx: Receiver<ThumbnailJob>, ready_tx: Sender<ThumbnailReady>) {
    while let Ok(job) = job_rx.recv() {
        let png_bytes = if let Ok(existing) = fs::read(&job.png_path)
            && existing.starts_with(&[0x89, b'P', b'N', b'G'])
        {
            existing
        } else {
            let rgba = render_asset_thumbnail(&job);
            let encoded = encode_rgba_png(&rgba);
            if let Some(parent) = job.png_path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let _ = fs::write(&job.png_path, &encoded);
            encoded
        };

        let _ = ready_tx.send(ThumbnailReady {
            uuid: job.uuid,
            source_hash: job.source_hash,
            png_bytes,
        });
    }
}

fn encode_rgba_png(rgba: &RgbaImage) -> Vec<u8> {
    let mut out = Vec::new();
    let dyn_img = DynamicImage::ImageRgba8(rgba.clone());
    if dyn_img
        .write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
        .is_ok()
    {
        out
    } else {
        Vec::new()
    }
}

fn render_asset_thumbnail(job: &ThumbnailJob) -> RgbaImage {
    match job.kind {
        AssetKind::Image => render_image_thumbnail(&job.abs_path)
            .unwrap_or_else(|| render_badge_thumbnail(job.kind)),
        AssetKind::GltfModel => render_gltf_thumbnail(&job.abs_path)
            .unwrap_or_else(|| render_badge_thumbnail(job.kind)),
        AssetKind::Tilemap => {
            render_tilemap_thumbnail(&job.abs_path, job.shared_registry.as_ref())
                .unwrap_or_else(|| render_badge_thumbnail(job.kind))
        }
        AssetKind::Prefab
        | AssetKind::Scene
        | AssetKind::Script
        | AssetKind::Material
        | AssetKind::Audio
        | AssetKind::Unknown => render_badge_thumbnail(job.kind),
    }
}

fn render_image_thumbnail(abs_path: &Path) -> Option<RgbaImage> {
    let decoded = image::open(abs_path).ok()?.to_rgba8();
    let src_w = decoded.width().max(1);
    let src_h = decoded.height().max(1);

    let max_inner = THUMBNAIL_SIZE - 12;
    let scale = (max_inner as f32 / src_w as f32).min(max_inner as f32 / src_h as f32);
    let dst_w = ((src_w as f32 * scale).round() as u32).clamp(1, max_inner);
    let dst_h = ((src_h as f32 * scale).round() as u32).clamp(1, max_inner);

    let resized = image::imageops::resize(&decoded, dst_w, dst_h, FilterType::Triangle);
    let mut canvas = make_checkerboard_canvas();
    let x_off = i64::from((THUMBNAIL_SIZE - dst_w) / 2);
    let y_off = i64::from((THUMBNAIL_SIZE - dst_h) / 2);
    image::imageops::overlay(&mut canvas, &resized, x_off, y_off);
    Some(canvas)
}

fn render_tilemap_thumbnail(
    abs_path: &Path,
    shared_registry: Option<&SharedAssetRegistry>,
) -> Option<RgbaImage> {
    let tilemap = load_tilemap_from_file(abs_path).ok()?;
    let tileset_img = tilemap
        .tileset_texture
        .and_then(|uuid| shared_registry?.resolve_abs_path(uuid))
        .and_then(|p| image::open(p).ok())
        .map(DynamicImage::into_rgba8);

    let mut canvas = make_solid_card(Rgba([24, 29, 38, 255]), Rgba([42, 165, 138, 255]));
    let visible_layers: Vec<_> = tilemap.layers.iter().filter(|l| l.visible).collect();
    if visible_layers.is_empty() {
        return Some(canvas);
    }

    let max_cols = visible_layers
        .iter()
        .map(|l| l.width)
        .max()
        .unwrap_or(1)
        .max(1);
    let max_rows = visible_layers
        .iter()
        .map(|l| l.height)
        .max()
        .unwrap_or(1)
        .max(1);

    let inner = (THUMBNAIL_SIZE - 16) as f32;
    let cell_px = (inner / max_cols as f32).min(inner / max_rows as f32);
    let grid_w = cell_px * max_cols as f32;
    let grid_h = cell_px * max_rows as f32;
    let start_x = (THUMBNAIL_SIZE as f32 - grid_w) * 0.5;
    let start_y = (THUMBNAIL_SIZE as f32 - grid_h) * 0.5;

    for layer in visible_layers {
        for row in 0..layer.height {
            for col in 0..layer.width {
                let idx = (row * layer.width + col) as usize;
                let Some(Some(tile_idx)) = layer.tiles.get(idx).copied() else {
                    continue;
                };
                let x0 = (start_x + col as f32 * cell_px).floor() as i32;
                let y0 = (start_y + row as f32 * cell_px).floor() as i32;
                let x1 = (start_x + (col + 1) as f32 * cell_px).ceil() as i32;
                let y1 = (start_y + (row + 1) as f32 * cell_px).ceil() as i32;

                let color = if let Some(img) = &tileset_img {
                    sample_tileset_cell_average(
                        img,
                        tilemap.grid_columns.max(1),
                        tilemap.grid_rows.max(1),
                        tile_idx,
                    )
                } else {
                    fallback_tile_color(tile_idx)
                };
                fill_rect(&mut canvas, x0, y0, x1.max(x0 + 1), y1.max(y0 + 1), color);
            }
        }
    }

    Some(canvas)
}

fn sample_tileset_cell_average(img: &RgbaImage, cols: u32, rows: u32, tile_idx: u32) -> Rgba<u8> {
    let col = tile_idx % cols;
    let row = (tile_idx / cols) % rows;
    let cell_w = (img.width() / cols).max(1);
    let cell_h = (img.height() / rows).max(1);
    let px = (col * cell_w + cell_w / 2).min(img.width().saturating_sub(1));
    let py = (row * cell_h + cell_h / 2).min(img.height().saturating_sub(1));
    *img.get_pixel(px, py)
}

fn fallback_tile_color(tile_idx: u32) -> Rgba<u8> {
    match tile_idx % 6 {
        0 => Rgba([72, 168, 96, 255]),
        1 => Rgba([128, 134, 145, 255]),
        2 => Rgba([58, 138, 210, 255]),
        3 => Rgba([188, 102, 72, 255]),
        4 => Rgba([206, 174, 84, 255]),
        _ => Rgba([144, 104, 186, 255]),
    }
}

fn render_gltf_thumbnail(abs_path: &Path) -> Option<RgbaImage> {
    let bytes = fs::read(abs_path).ok()?;
    let gltf = gltf::Gltf::from_slice(&bytes).ok()?;
    let base_dir = abs_path.parent();

    let mut buffers: Vec<Vec<u8>> = Vec::new();
    for buffer in gltf.buffers() {
        match buffer.source() {
            gltf::buffer::Source::Bin => {
                buffers.push(gltf.blob.clone().unwrap_or_default());
            }
            gltf::buffer::Source::Uri(uri) => {
                if let Some((_meta, b64)) = uri.split_once(";base64,") {
                    buffers.push(BASE64_STANDARD.decode(b64).unwrap_or_default());
                } else if let Some(dir) = base_dir {
                    buffers.push(fs::read(dir.join(uri)).unwrap_or_default());
                } else {
                    buffers.push(Vec::new());
                }
            }
        }
    }

    let mut triangles: Vec<[Vec3; 3]> = Vec::new();
    for mesh in gltf.meshes() {
        for prim in mesh.primitives() {
            let reader = prim.reader(|buf| buffers.get(buf.index()).map(Vec::as_slice));
            let Some(positions_iter) = reader.read_positions() else {
                continue;
            };
            let positions: Vec<Vec3> = positions_iter.map(Vec3::from_array).collect();
            if positions.is_empty() {
                continue;
            }
            if let Some(ReadIndices::U16(idx_iter)) = reader.read_indices() {
                let idxs: Vec<u16> = idx_iter.collect();
                for tri in idxs.chunks_exact(3) {
                    if let (Some(&a), Some(&b), Some(&c)) = (
                        positions.get(tri[0] as usize),
                        positions.get(tri[1] as usize),
                        positions.get(tri[2] as usize),
                    ) {
                        triangles.push([a, b, c]);
                    }
                }
            } else if let Some(ReadIndices::U32(idx_iter)) = reader.read_indices() {
                let idxs: Vec<u32> = idx_iter.collect();
                for tri in idxs.chunks_exact(3) {
                    if let (Some(&a), Some(&b), Some(&c)) = (
                        positions.get(tri[0] as usize),
                        positions.get(tri[1] as usize),
                        positions.get(tri[2] as usize),
                    ) {
                        triangles.push([a, b, c]);
                    }
                }
            } else {
                for tri in positions.chunks_exact(3) {
                    triangles.push([tri[0], tri[1], tri[2]]);
                }
            }
        }
    }

    if triangles.is_empty() {
        return None;
    }

    let mut min_pt = Vec3::splat(f32::MAX);
    let mut max_pt = Vec3::splat(f32::MIN);
    for tri in &triangles {
        for &v in tri {
            min_pt = min_pt.min(v);
            max_pt = max_pt.max(v);
        }
    }
    let center = (min_pt + max_pt) * 0.5;
    let extent = (max_pt - min_pt).max_element().max(0.001);
    let inv_scale = 1.65 / extent;

    let yaw = 0.62_f32;
    let pitch = -0.45_f32;
    let (sy, cy) = bevy_math::ops::sin_cos(yaw);
    let (sp, cp) = bevy_math::ops::sin_cos(pitch);
    let project = |p: Vec3| -> Vec3 {
        let q = (p - center) * inv_scale;
        let x1 = q.x * cy - q.z * sy;
        let z1 = q.x * sy + q.z * cy;
        let y2 = q.y * cp - z1 * sp;
        let z2 = q.y * sp + z1 * cp;
        Vec3::new(
            (x1 * 0.42 + 0.5) * THUMBNAIL_SIZE as f32,
            (-y2 * 0.42 + 0.5) * THUMBNAIL_SIZE as f32,
            z2,
        )
    };

    let mut canvas = make_solid_card(Rgba([24, 27, 36, 255]), Rgba([92, 168, 255, 255]));
    let mut zbuf = vec![f32::NEG_INFINITY; (THUMBNAIL_SIZE * THUMBNAIL_SIZE) as usize];
    let light_dir = Vec3::new(0.45, 0.78, 0.52).normalize();

    for tri in &triangles {
        let world_n = (tri[1] - tri[0])
            .cross(tri[2] - tri[0])
            .normalize_or(Vec3::Y);
        let ndl = world_n.dot(light_dir).abs().clamp(0.18, 1.0);
        let shade = 0.28 + 0.72 * ndl;
        let color = Rgba([
            (110.0 * shade) as u8,
            (188.0 * shade) as u8,
            (248.0 * shade) as u8,
            255,
        ]);

        let p0 = project(tri[0]);
        let p1 = project(tri[1]);
        let p2 = project(tri[2]);
        rasterize_triangle(&mut canvas, &mut zbuf, p0, p1, p2, color);
    }

    Some(canvas)
}

fn rasterize_triangle(
    canvas: &mut RgbaImage,
    zbuf: &mut [f32],
    a: Vec3,
    b: Vec3,
    c: Vec3,
    color: Rgba<u8>,
) {
    let min_x = a.x.min(b.x).min(c.x).floor().max(2.0) as i32;
    let max_x = a
        .x
        .max(b.x)
        .max(c.x)
        .ceil()
        .min((THUMBNAIL_SIZE - 3) as f32) as i32;
    let min_y = a.y.min(b.y).min(c.y).floor().max(2.0) as i32;
    let max_y = a
        .y
        .max(b.y)
        .max(c.y)
        .ceil()
        .min((THUMBNAIL_SIZE - 3) as f32) as i32;

    let area = edge_fn(a.x, a.y, b.x, b.y, c.x, c.y);
    if area.abs() < 1e-4 {
        return;
    }

    for py in min_y..=max_y {
        for px in min_x..=max_x {
            let x = px as f32 + 0.5;
            let y = py as f32 + 0.5;
            let w0 = edge_fn(b.x, b.y, c.x, c.y, x, y) / area;
            let w1 = edge_fn(c.x, c.y, a.x, a.y, x, y) / area;
            let w2 = edge_fn(a.x, a.y, b.x, b.y, x, y) / area;
            if w0 >= -1e-4 && w1 >= -1e-4 && w2 >= -1e-4 {
                let z = w0 * a.z + w1 * b.z + w2 * c.z;
                let idx = (py as u32 * THUMBNAIL_SIZE + px as u32) as usize;
                if z > zbuf[idx] {
                    zbuf[idx] = z;
                    canvas.put_pixel(px as u32, py as u32, color);
                }
            }
        }
    }
}

fn edge_fn(ax: f32, ay: f32, bx: f32, by: f32, cx: f32, cy: f32) -> f32 {
    (cx - ax) * (by - ay) - (cy - ay) * (bx - ax)
}

fn render_badge_thumbnail(kind: AssetKind) -> RgbaImage {
    let accent = match kind {
        AssetKind::Image => Rgba([88, 178, 255, 255]),
        AssetKind::GltfModel => Rgba([144, 126, 255, 255]),
        AssetKind::Prefab => Rgba([78, 198, 224, 255]),
        AssetKind::Scene => Rgba([244, 184, 72, 255]),
        AssetKind::Tilemap => Rgba([72, 206, 152, 255]),
        AssetKind::Script => Rgba([234, 120, 168, 255]),
        AssetKind::Material => Rgba([196, 144, 248, 255]),
        AssetKind::Audio => Rgba([93, 214, 202, 255]),
        AssetKind::Unknown => Rgba([142, 150, 165, 255]),
    };

    let mut canvas = make_solid_card(Rgba([28, 32, 42, 255]), accent);
    let mid = THUMBNAIL_SIZE as i32 / 2;

    match kind {
        AssetKind::Prefab => {
            fill_rect(&mut canvas, mid - 26, mid - 26, mid + 26, mid + 26, accent);
            fill_rect(
                &mut canvas,
                mid - 16,
                mid - 16,
                mid + 16,
                mid + 16,
                Rgba([28, 32, 42, 255]),
            );
            fill_rect(&mut canvas, mid - 8, mid - 8, mid + 8, mid + 8, accent);
        }
        AssetKind::Scene => {
            fill_rect(&mut canvas, mid - 30, mid - 20, mid + 30, mid + 24, accent);
            fill_rect(
                &mut canvas,
                mid - 24,
                mid - 14,
                mid + 24,
                mid + 18,
                Rgba([28, 32, 42, 255]),
            );
            fill_rect(&mut canvas, mid - 30, mid - 28, mid + 30, mid - 20, accent);
        }
        AssetKind::Script => {
            fill_rect(&mut canvas, mid - 28, mid - 24, mid - 12, mid - 16, accent);
            fill_rect(&mut canvas, mid - 28, mid - 4, mid + 20, mid + 4, accent);
            fill_rect(&mut canvas, mid - 28, mid + 14, mid + 8, mid + 22, accent);
        }
        AssetKind::GltfModel => {
            fill_rect(&mut canvas, mid - 22, mid - 22, mid + 22, mid + 22, accent);
            fill_rect(
                &mut canvas,
                mid - 12,
                mid - 12,
                mid + 28,
                mid + 28,
                Rgba([92, 214, 248, 200]),
            );
        }
        _ => {
            fill_rect(&mut canvas, mid - 20, mid - 24, mid + 20, mid + 24, accent);
            fill_rect(
                &mut canvas,
                mid - 12,
                mid - 16,
                mid + 12,
                mid + 16,
                Rgba([28, 32, 42, 255]),
            );
        }
    }
    canvas
}

fn make_checkerboard_canvas() -> RgbaImage {
    let mut canvas = RgbaImage::new(THUMBNAIL_SIZE, THUMBNAIL_SIZE);
    for y in 0..THUMBNAIL_SIZE {
        for x in 0..THUMBNAIL_SIZE {
            let border = x < 2 || y < 2 || x >= THUMBNAIL_SIZE - 2 || y >= THUMBNAIL_SIZE - 2;
            let color = if border {
                Rgba([54, 62, 76, 255])
            } else if ((x / 16) + (y / 16)).is_multiple_of(2) {
                Rgba([30, 34, 43, 255])
            } else {
                Rgba([39, 44, 56, 255])
            };
            canvas.put_pixel(x, y, color);
        }
    }
    canvas
}

fn make_solid_card(bg: Rgba<u8>, border: Rgba<u8>) -> RgbaImage {
    let mut canvas = RgbaImage::new(THUMBNAIL_SIZE, THUMBNAIL_SIZE);
    for y in 0..THUMBNAIL_SIZE {
        for x in 0..THUMBNAIL_SIZE {
            let is_border = x < 3 || y < 3 || x >= THUMBNAIL_SIZE - 3 || y >= THUMBNAIL_SIZE - 3;
            canvas.put_pixel(x, y, if is_border { border } else { bg });
        }
    }
    canvas
}

fn fill_rect(canvas: &mut RgbaImage, x0: i32, y0: i32, x1: i32, y1: i32, color: Rgba<u8>) {
    let min_x = x0.clamp(0, THUMBNAIL_SIZE as i32) as u32;
    let max_x = x1.clamp(0, THUMBNAIL_SIZE as i32) as u32;
    let min_y = y0.clamp(0, THUMBNAIL_SIZE as i32) as u32;
    let max_y = y1.clamp(0, THUMBNAIL_SIZE as i32) as u32;
    for y in min_y..max_y {
        for x in min_x..max_x {
            canvas.put_pixel(x, y, color);
        }
    }
}
