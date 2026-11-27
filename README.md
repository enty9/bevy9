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
- **Игровые подсистемы (этап 5)**
  - Enhanced Input: `.input.ron` с действиями, осями, контекстами и переназначением клавиш через **Gameplay**. Состояния доступны в `Update` и `FixedUpdate`.
  - Rapier 2D/3D: тела и коллайдеры, суставы, сенсоры, лучи и свипы форм, контроллеры персонажа, фиксированный шаг и отладочные контуры.
  - Kira: шины Master/SFX/Music/UI/Voice, 2D/3D-источники, затухание по дистанции, стриминг и предпрослушивание из Content Browser.

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
    │   ├── engine_input/         # Input Actions, контексты и сохранение привязок
    │   ├── engine_physics/       # Независимые миры Rapier 2D и 3D
    │   ├── engine_audio/         # Микшер и пространственное аудио Kira
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

## Проверка этапа 5 в редакторе

1. Откройте `sandbox_2d` или `sandbox_3d` и включите **Gameplay → Viewport collider outlines**. Сравните контуры пола, динамических тел и `Trigger zone` с геометрией сцены. Чтобы увидеть падение и отскок, запустите проект в отдельном окне с `--physics-debug`:

   ```bash
   cargo run --manifest-path engine/Cargo.toml -p engine_runtime -- --project engine/samples/sandbox_3d --physics-debug
   ```

2. Выберите объект во вьюпорте. Клавиша `F` фокусирует камеру; переназначьте `editor_focus` в **Gameplay → Input bindings** и проверьте новую привязку. После нажатия на текущую привязку можно нажать клавишу или выбрать кнопку мыши, кнопку либо ось геймпада. Кнопка **Reset** восстанавливает авторскую привязку. Настройки сохраняются отдельно для проекта; для этого у проекта должен быть постоянный `project.id`. `project.input_mapping` указывается относительно `assets_dir`.
3. Импортируйте WAV, OGG/Vorbis, MP3 или FLAC. Выберите клип в **Content Browser → Audio → Play preview** и измените уровень `UI` или `MASTER` в **Gameplay → Audio mixer**. Если нужен тестовый файл: `python3 engine/samples/make_audio_preview.py /tmp/bevy9-preview.wav`.
4. Для пространственного звука добавьте `AudioEmitter` объекту, выберите его `clip` и установите `Scene2D` или `Scene3D`. Разместите `AudioListener` на игровой камере (в сэмплах он уже есть) и нажмите **Preview emitter** в Inspector. Перемещайте объект и меняйте громкость выбранной шины. `min_distance` и `max_distance` задаются в единицах сцены; у 2D-спрайтов это обычно пиксели. Чтобы источник включался в standalone, задайте `autoplay`.

В редакторе коллайдеры выбранных объектов видны и при выключенном глобальном показе контуров. Content Browser использует шину UI, а предпрослушивание `AudioEmitter` — шину самого источника. Симуляция и автозапуск источников доступны в standalone; Play-In-Editor относится к этапу 6.

Для игрового кода используйте `ActionState::get` в `Update` или `FixedActionState::get` в `FixedUpdate`, `PhysicsWorld2D` / `PhysicsWorld3D` для запросов и импульсов, `CharacterMotion2D` / `CharacterMotion3D` для управления кинематическим персонажем. `CollisionEvent` и `TriggerEvent` доступны через ECS Observers.

---

## Проверки и тесты

```bash
# Тесты Rust workspace
cargo test --manifest-path engine/Cargo.toml -j 2 --workspace
cargo test --manifest-path engine/Cargo.toml -p engine_input -p engine_physics -p engine_audio

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
