# Bevy9 Engine & Editor

2D/3D игровой движок и десктопный визуальный редактор на базе **Rust + Bevy** (`0.20.0-dev` в корне репозитория), **Bevy Remote Protocol (BRP + WebSocket IPC)** и **Tauri v2 + React/TypeScript**.

> Подробная архитектура и поэтапная дорожная карта находятся в [PLAN.md](./PLAN.md).

---

## Возможности (реализовано на текущий момент)

- **Ядро и проекты (`engine_core`, `engine_runtime`)**
  - Манифест проекта `project.toml`, `ProjectPaths` (`assets/`, `scenes/`, `scripts/`), стабильные `EntityGuid` и `AssetUuid`, режимы `2d` / `3d` / `hybrid`.
  - Запуск runtime в режимах `--headless`, обычном окне (`Window`) и хосте редактора (`Editor`).
- **Сериализация сцен и префабов (`engine_scene`)**
  - Версионированный формат `.level.ron` для 2D/3D-сцен, описателей примитивов, PBR-материалов, спрайтов, света, камер, `SceneGltfModel` и `SceneTilemap`.
  - Многоразовые префабы `.prefab.ron` (`PrefabInstance` / `PrefabMember`) с отслеживанием переопределений, `Apply` и `Revert`.
- **Ресурсы, кэш и 2D/3D-импорт (`engine_assets`)**
  - Индексация `assets/` в `AssetRegistry` и сохранение стабильных UUID и настроек импорта в sidecar-файлах `*.b9meta` (BLAKE3-хэши, граф зависимостей, безопасное перемещение и удаление, кастомный `uuid://` источник и file watcher).
  - Поддержка 2D-текстур (`Linear`/`Nearest`, sRGB, спрайт-атласы `TextureAtlasLayout`), 3D `glTF`/`.glb` и 2D тайлмапов `.tilemap.ron` (`TilemapAsset`).
  - Локальный Derived Data Cache в `.cache/ddc/` (`processed/`, `thumbnails/`, `processor.log`) с асинхронной генерацией 128×128 PNG-миниатюр.
- **Редактор Tauri v2 + React (`engine/editor`)**
  - Потоковый **Offscreen Viewport** через RGBA8-буфер по WebSocket IPC + отсоединяемый **Native Window** режим, 3D/2D навигация камеры, GPU/raycast picking и `W`/`E`/`R` гизмо (`World`/`Local`, snap-шаги).
  - **World Outliner** (иерархия, поиск, Drag-and-Drop переподчинение, пресеты и префабы), **Inspector** по схеме Bevy `registry.schema`, **Undo/Redo** (`Ctrl+Z` / `Ctrl+Y`).
  - **Content Browser** (папки, фильтры, миниатюры, настройки импорта, импорт кнопкой и OS Drag-and-Drop, перетаскивание ассетов во Viewport и в поля `AssetUuid` в Inspector).

---

## Структура репозитория

```text
bevy9/
├── crates/bevy_*             # Модули upstream Bevy (используются по path-зависимостям без правок)
├── PLAN.md                   # Архитектурная дорожная карта проекта
└── engine/                   # Изолированный workspace Bevy9 Engine & Editor
    ├── Cargo.toml
    ├── crates/
    │   ├── engine_core/          # Идентификаторы EntityGuid/AssetUuid, ProjectManifest, расписание EngineSet
    │   ├── engine_protocol/      # JSON-RPC (BRP) контракты, WebSocket IPC сообщения и бинарный фрейм-кодек
    │   ├── engine_assets/        # AssetRegistry, *.b9meta, importers, TilemapAsset, .cache/ddc, ThumbnailService
    │   ├── engine_scene/         # .level.ron, .prefab.ron, 2D/3D/glTF/Tilemap описатели и гидратация
    │   ├── engine_render/        # Offscreen render-target readback, камеры редактора, picking, 2D/3D gizmos
    │   ├── engine_editor_bridge/ # Кастомные BRP-методы engine.* и WebSocket IPC сервер
    │   └── engine_runtime/       # Бинарник движка (Headless / Window / Editor Host)
    ├── editor/
    │   ├── src-tauri/            # Tauri v2 оболочка и супервизор процесса engine_runtime
    │   └── ui/                   # React + TypeScript UI редактора (Vite)
    └── samples/
        ├── sandbox_3d/           # Демо 3D-проекта (шахматная текстура, .glb кристалл, сцена main.level.ron)
        └── sandbox_2d/           # Демо 2D-проекта (пиксель-арт тайлсет 2x2, .tilemap.ron, 2D сцена)
```

---

## Быстрый старт

### Требования

- **Rust** (stable toolchain согласно `rust-version` в [engine/Cargo.toml](engine/Cargo.toml)) + `cargo-tauri` (`cargo install tauri-cli` при необходимости)
- **Node.js** и **pnpm**
- **Linux**: системные пакеты для Bevy (Vulkan/Wayland/X11/ALSA) и Tauri (`webkit2gtk-4.1`, `gtk3`, `librsvg`)

### Установка зависимостей UI

```bash
pnpm --dir engine/editor/ui install
```

### Запуск визуального редактора (Tauri v2)

```bash
cd engine/editor/src-tauri && cargo tauri dev
```

Редактор автоматически соберёт/запустит `engine_runtime` как дочерний Editor Host и откроет проект из `engine/samples/`. Переключаться между `sandbox_3d`, `sandbox_2d` или открыть произвольный проект с `project.toml` можно прямо из верхней панели.

### Запуск standalone `engine_runtime`

```bash
# 3D-сцена в окне
cargo run --manifest-path engine/Cargo.toml -p engine_runtime -- --project engine/samples/sandbox_3d

# 2D-сцена в окне
cargo run --manifest-path engine/Cargo.toml -p engine_runtime -- --project engine/samples/sandbox_2d

# Headless прогон нескольких кадров (для CI / smoke-проверок)
cargo run --manifest-path engine/Cargo.toml -p engine_runtime -- --project engine/samples/sandbox_3d --headless --max-frames 5
```

---

## Основные горячие клавиши редактора

- **Viewport (3D)**: `ПКМ + WASD / Q / E` — свободный полёт, `Alt + ЛКМ` / `СКМ` — вращение/панорама, колесо мыши — зум камеры, `F` — фокус на выбранном объекте
- **Viewport (2D)**: `ПКМ` / `СКМ` — панорамирование, колесо мыши — масштабирование камеры
- **Трансформация**: `W` — перемещение, `E` — поворот, `R` — масштаб, `Ctrl` (удержание) — привязка к сетке
- **Правка и файлы**: `Ctrl+Z` / `Ctrl+Y` (`Ctrl+Shift+Z`) — Undo / Redo, `Ctrl+S` — сохранить сцену, `Delete` — удалить выбранный узел

---

## Проверки и тесты

```bash
# Тесты Rust workspace
cargo test --manifest-path engine/Cargo.toml -j 2 --workspace

# Линтинг без предупреждений
cargo clippy --manifest-path engine/Cargo.toml --workspace --all-targets -- -D warnings

# Проверка Tauri-обёртки и React/TypeScript UI
cargo check --manifest-path engine/editor/src-tauri/Cargo.toml
pnpm --dir engine/editor/ui typecheck
pnpm --dir engine/editor/ui build
```

---

## Лицензия

Движок Bevy в составе репозитория распространяется под двойной лицензией **MIT / Apache-2.0** ([LICENSE-MIT](LICENSE-MIT), [LICENSE-APACHE](LICENSE-APACHE)). Код Bevy9 под каталогом `engine/` использует ту же совместимую модель лицензирования `MIT OR Apache-2.0`.
