# Рефакторинг: новая архитектура

Ядро без языков, модули — только wasm, Host — технический прокси. Обсуждено после 3.7.0
(до переписывания редактора) и 04.11. Пункт 33 в `BACKLOG.md` ссылается сюда.

**Главное правило: клиентов пока нет — совместимость ломаем свободно.** Без миграций,
переходных слоёв, поддержки старых версий ABI, данных и ключей хранилища.

## Зачем

- Разработчики модулей не могут сделать свой язык: подсветка и подсказки живут в ядре
  (BACKLOG 31). Пример — Redis/Valkey хотят позиционные подсказки, как в MySQL.
- Исходная модель — Ports & Adapters (hexagonal) + микроядро/плагины
  (`docs/superpowers/specs/2026-09-26-service-platform-design.md`, добавлен в ffe2131, удалён
  в 1.0.9 / 21348b1) — размылась: точки расширения плагинов переехали в ядро, web-адаптеры
  удалены, CLI остался.
- Имена — наследие Active Console (`console` везде), логика размазана по `app.rs` и панелям.

## Что есть сейчас

### Папки и крейты

```
adapters/
  extension/        Chrome extension: manifest.json, devtools.html/js, panel.html/js
  cli/              README и тесты CLI
rust/
  native/           services-native — native host
    src/adapters/   native.rs, framing.rs (native messaging), cli.rs (CLI)
    src/core/       registry, modules, install, sources, variables (Dictionary), history,
                    templates, hints, skills, updates, operations, process
    src/host.rs     функции для модулей (host.proto): HTTP через общие reqwest-клиенты,
                    папка данных, логи
    src/runtime.rs  выполнение запросов, ответы консоли
    src/agent.rs    Agent на Copilot SDK; agent/skills.rs
    src/shell_environment.rs
  ui/               services-ui — приложение (Yew, wasm)
    src/*.rs        логика без DOM: console, checks, document, discovery, sessions, format,
                    tables, find, profiles, storage, logfmt, activity, agent_workspace;
                    языки: completion, sql_completion, graphql_completion,
                    template_completion, syntax
    src/editor/     модель редактора (state, commands, history, fold, highlight, layout,
                    text, brackets, checks, session) и токенайзеры sql.rs, json.rs
    src/browser/    Yew-компоненты: app.rs (2610 строк), app/console_panel.rs, views,
                    agent_panel, agent_settings, console_variables, history, templates,
                    sessions, module_settings, sources_settings, updates, find, check_rail,
                    pane_resizer, page_agent, transport, module_ui, session_database, editor/
    assets/         css
  contracts/        services-contracts — типы App ↔ Host, manifest, package, updates
  module-api/       services-module-api — Host ↔ модуль: gRPC-подобный транспорт через
                    границу dylib (stabby), host.proto / module.proto, SDK модуля
  module-ui/        services-module-ui — трейт ServiceUi (check, request, import, export)
                    для ui.wasm модуля, ABI services_ui_*
  modules/<name>/   rest, graphql, grpc, mysql, splunk, artemis, opensearch, terminal:
                    dylib (src/), ui/ (ui.wasm), manifest.json, skill/, RELEASE_NOTES.md
  examples/echo/    пример модуля
  installer/        services-installer
  testkit/          services-testkit
  xtask/            сборка, тесты, браузерные сценарии
modules/            собранные пакеты модулей (Стандартный источник)
dist/ docs/ dev/ scripts/ install.sh prerelease
```

### Связи

- Панель (app.wasm) ↔ Host: Chrome native messaging, один порт, один писатель
  (`native.rs`), кадры (`framing.rs`).
- Host загружает `module.dylib` (stabby, `module.proto`). Модуль вызывает Host
  (`host.proto`): HTTP, папка данных, логи.
- App берёт у Host `ui.wasm` модуля по хешу, кеширует в IndexedDB и вызывает `ServiceUi`:
  секции → запрос, import/export.
- Язык: manifest объявляет `language` секции из фиксированного списка
  (`contracts/src/manifest.rs`), `keywords`, `hints` и discovery одного из видов
  `DiscoveryKind` (openapi, graphql, grpc-reflection, sql, queues). Подсветка
  (`editor/highlight.rs`, `sql.rs`, `json.rs`, `syntax.rs`) и подсказки (`completion.rs`,
  `sql_completion.rs`, `graphql_completion.rs`) — в ядре App.
- Agent работает в Host (Copilot SDK); его инструментам нужны данные App
  (`agent_workspace.rs`, `page_agent.rs`).

### Проблемы

- Языки в ядре: модуль не может сделать свои подсветку и подсказки.
- Две точки входа: расширение и CLI; папка `adapters/` — от старой модели.
- Нативный код в каждом модуле (dylib + stabby), редкий сбой stabby (BACKLOG 9).
- Префикс `console` везде: `ConsolePanel`, `console_panel.rs`, `ConsoleVariables`,
  CSS-классы `console-*`, ключи хранилища `servicesConsole.*`.
- Variables и Dictionary — один компонент `ConsoleVariables` с флагом `dictionary`.
- Нет разделения Kernel / Shell: логика в `app.rs` и панелях.
- Мало общих виджетов: вкладки, списки, формы, шапка и низ панели у каждой панели свои.
- Раздел архитектуры в `rust/native/README.md` устарел.
- Корневая `modules/` занята собранными пакетами.

## Решено

- Одна точка входа: расширение Chrome + Host. Убрать `adapters/` и CLI
  (`native/src/adapters/cli.rs` и тесты на нём).
- Ядро не владеет ни одним языком (SQL, GraphQL и т. д.). Модуль сам отвечает за свой язык:
  подсветка, подсказки, диагностика. Один API, без режимов; существующие модули
  переписываем. Закрывает BACKLOG 31.
- Host — технический прокси: драйверы протоколов (http, grpc, mysql, process) плюс сырой
  tcp+TLS как запасной выход. Все внешние вызовы идут через него. Модули — только wasm, без
  dylib и stabby.
- App — основное приложение: Shell, Widgets, Kernel, Modules (сервисы и панели,
  загружаемые как wasm).
- Модуль строит UI из общих виджетов и не рисует DOM сам.
- Variables — модуль с вкладом «преобразование запроса».
- Модуль получает от ядра события взаимодействия (клик, ввод текста, ручной discovery,
  пришёл ответ или ошибка) из редактора или Agent и сам решает, когда и как делать
  discovery (может пользоваться рекомендациями и библиотеками ядра).
- `contracts`, `module-api`, `module-ui` — отдельный API-слой.

### Имена и слои (04.11)

- **App** (фронтенд, всё в Chrome):
  - Extension — связь с Chrome (manifest, devtools-страница, загрузка wasm);
  - Kernel — логика без DOM (сессии, чеки, запросы через Host, события модулям, загрузка
    модулей);
  - Shell — рамка UI;
  - Widgets — общие компоненты (Editor, FindBar, CheckRail, StatusBar, Tree, Resizer,
    Markdown);
  - Modules — wasm-модули: сервисы и панели.
- **Host** (бэкенд, процесс на машине):
  - Bridge — native messaging с расширением;
  - Drivers — http, grpc, mysql, process, tcp+TLS;
  - Files — источники и конфиги, история, переменные, установка модулей, обновления;
  - Agent runtime — сессия Copilot SDK.
- Между ними — **Protocol** (сейчас `contracts`). Отдельно: **SDK** для модулей
  (`module-api` + `module-ui`), **Installer**, **Tooling** (`xtask`, `testkit`).
- UI:
  - Top bar: Service tabs, Session name, Toolbar.
  - Workspace: Editor, Check rail, Status bar.
  - Side panel: History, Variables, Dictionary, Templates, Hints, Agent, Sessions, Settings
    (вкладки Modules, Sources, Agent).
  - Banner, Overlay (виджеты): статус Host, ошибки, экран обновления.
  - Документ: Check, Section, Options, Response.
- Префикс `console` (наследие Active Console) убрать везде: `ConsolePanel` → `SidePanel`,
  `console_panel.rs` → `workspace.rs` и т. д.
- Variables и Dictionary — разные панели и визуально, и в коде: два отдельных компонента
  вместо `ConsoleVariables` с флагом `dictionary`.
- Страница App `panel.html`/`panel.js` → `services.html`/`services.js`, как вкладка
  «Services» в DevTools. `panel.html` удаляется сразу, без перенаправления; адрес
  отдельной вкладки в README — `chrome-extension://<id>/services.html`. ID расширения не
  меняется.
- Модули Rust-кода — в стиле 2018: корень модуля `bridge.rs` рядом с папкой `bridge/`, без
  `mod.rs`. Нынешние `mod.rs` переименовываются.
- Слово «профиль» убрать (в коде, UI, документации, скилах Agent): **Variables** —
  переменные (сейчас профиль `var`), **Environment** — переменные окружения (профиль `env`),
  **Dictionary** — справочник из **наборов** (`set`; сейчас «профили Dictionary»: `dev`,
  `stage`…). Синтаксис прежний: `{{env:X}}`, `{{dev:HOST}}` — из набора `dev`, `{{:x}}` —
  Variables → наборы Dictionary по порядку → Environment. Формат источника:
  `dictionary.json` `{"sets": {"<набор>": {"<VAR>": "value"}}}`, старый `profiles` читается
  при переходе.

### Host (04.11)

- Протоколы — только в Host: браузер не даёт сокетов и процессов, а `fetch` не умеет свой
  TLS, часть заголовков, ручные редиректы и trailers gRPC. Модуль описывает вызов, Host
  исполняет.
- Драйверы минимальные и не знают сервисов: ничего под конкретный модуль. Темп Splunk
  (13 с на сервер) уходит из http в модуль Splunk: Kernel даёт модулям общую блокировку
  (Web Locks API между экземплярами App одного расширения) и общее хранилище.
- Один экземпляр App (вкладка DevTools или отдельная вкладка) — один порт — один процесс
  Host. Окон внутри Host нет; `core/operations.rs` (окна от удалённого web-адаптера) — мёртвый
  код, удаляется. Отмена — по `Cancel(id)`, закрытый stdin отменяет всё.
- Переменные (`{{env:X}}`, `{{profile:x}}`, `{{:x}}`) подставляются в App (панель Variables,
  см. «Kernel и панели») перед вызовом драйвера; Host получает готовый запрос. Окружение для
  `env` берётся у Host (`system.rs`). Маскировка значений в истории (сейчас `history.rs`
  `redactor`) — тоже в App.
- Host не знает форматов панелей (History, Variables, Dictionary, Templates, Hints): `files`
  даёт общие механизмы, формат и логика — у панели-модуля:
  - `files.rs` — пути в папке данных, уведомления об изменениях другим процессам Host;
  - `db.rs` — одна sqlite (WAL): транзакции и межпроцессные блокировки вместо своих
    (`write_atomic` в `sources.rs`, блокировки в `sources.rs`, `templates.rs`,
    `.profile-stores.lock`; hints без атомарной записи). Таблица kv (владелец — id модуля
    или панели, ключ, документ): сессии (переезжают из IndexedDB, `session_database.rs`),
    Variables, наборы Dictionary, настройки модулей и панелей (модель и Reasoning Agent,
    вкладка Settings), чаты Agent, черновики, локальные правки конфигов. Таблицы History
    (запись, поиск). Только постоянное: папка данных переживает удаление расширения (Chrome
    стирает его storage), без квот. Временное (курсор, undo, идущий запрос, discovery-кеш
    модуля, кеш списка модулей) — в памяти/хранилище App. Нынешние файлы
    (`.variable-profiles.json`, `.dictionary-profiles.json`, `.console-hints.txt`,
    `.console-templates.json`, `.console-history.sqlite3`, localStorage/IndexedDB) мигрируют
    в базу;
  - слоёв в Host нет: `sources.rs` отдаёт сырые документы источников по порядку
    (`[(источник, документ)]`), локальные правки — запись в db, а складывает слои (правило
    перекрытия, происхождение, «сбросить к источнику») панель в App; общий помощник — в SDK.
    Исключение — скилы: их читает Agent в Host;
  - `modules.rs`, `sources.rs`, `updates.rs`.
- Модули не запрашивают разрешений на драйверы.
- Общее у драйверов:
  - `drivers/net.rs` — один сетевой слой: DNS, connect, TLS (системные корни, свой CA,
    клиентский сертификат, insecure), прокси, connect-таймаут. На нём http, grpc, mysql и
    tcp; `tcp.rs` — тонкая обёртка. Сейчас TLS у каждого свой (reqwest, tonic
    `with_native_roots`, rustls в mysql), своего CA и сертификата нет нигде.
  - Один http-клиент вместо четырёх по флагам (`host.rs` `clients`): без авторедиректов,
    драйвер проходит их сам, каждый переход виден в Debug.
  - Кеш соединений в `drivers.rs` по (драйвер, адрес, TLS) с закрытием по простою — вместо
    пулов mysql в модуле и нового канала grpc на каждый вызов.
  - Таймауты (`connect_ms`, `timeout_ms`) — в каждом вызове, умолчания в одном месте, а не
    зашиты в драйверах (grpc 5 с / 120 с, mysql KILL 2 с).
  - Одна ошибка: `DriverFailure { stage: dns | connect | tls | send | timeout | status |
    decode | cancelled, causes }` вместо `HttpFailure` только у http.
  - Debug у всех одинаковый: время по этапам (dns, connect, tls, первый байт, всего), адрес
    удалённой стороны.
  - `health` отдаёт список драйверов и их версии; модуль проверяет, что нужное есть.
- Agent работает, только пока открыт App: чеки выполняют модули в App.
- В `<data>/modules/<id>/` — собранный пакет (`package.json`, wasm, `RELEASE_NOTES.md`,
  скил); Host его не запускает, только хранит и отдаёт App. Исходники стандартных
  модулей — в репозитории.

```
host/src/
├── main.rs
├── bridge.rs          цикл stdin → router → stdout, один писатель, выход по закрытию stdin
├── bridge/
│   ├── framing.rs     кадры native messaging (4 байта длины + JSON), лимиты
│   ├── chunks.rs      ответы > 1 МиБ кусками по 128 КиБ, всего до 64 МиБ
│   ├── router.rs      service → drivers / files / agent / system, токены отмены по id
│   └── health.rs      версия протокола, ошибка старта (окружение shell)
├── drivers.rs         Call / стрим / отмена, кеш соединений, таймауты, DriverFailure, Debug
├── drivers/
│   ├── net.rs         DNS, connect, TLS, прокси — общий слой
│   ├── http.rs        запрос, потоковое тело; редиректы сам, повторы, статус
│   ├── grpc.rs        HTTP/2, TLS, metadata, deadline, unary и стримы
│   ├── mysql.rs       пул по DSN, query → наборы строк с типами, стрим, KILL QUERY
│   ├── process.rs     группа процессов, stdout/stderr/stdin, сигналы, дочитывание
│   └── tcp.rs         connect (+TLS), write, read-стрим, close — запасной выход
├── files.rs           пути, уведомления
├── files/             db (sqlite: kv + History), modules, sources, updates
├── agent.rs           Copilot CLI, сессии, ход, Stop, лимит 30 мин, INSTRUCTIONS
├── agent/
│   ├── tools.rs       регистрирует инструменты от App, проксирует вызовы
│   ├── skills.rs      папки скилов: корневой (от Kernel), модули, панели, источники
│   └── models.rs      список моделей, auto
└── system.rs          доступ к ОС: окружение login-shell, буфер обмена
```

grpc (05.11): Host — только транспорт байтами (HTTP/2, TLS, metadata, deadline, стримы).
`.proto`, JSON ↔ protobuf (prost-reflect) и reflection — в wasm-модуле grpc.

### Agent (04.11)

- Host не знает инструментов: в начале хода Kernel передаёт список инструментов со схемами
  и корневой скил, Host регистрирует их в SDK и проксирует вызовы в App. Сейчас 28
  инструментов и тексты скила зашиты в Host (`agent.rs`, `agent-skills/services-console/`).
- Инструменты — в Kernel: `agent/console.rs` (`console_*`) и `agent/page.rs` (`page_*`).
  Имена прежние; `console_skill` убирается — корневой скил грузит встроенный `skill`.
- Маскировка результатов — в Kernel (`agent/redact.rs`) до отправки в Host; `Redactor` из
  Host уходит.
- Корневой скил (общие правила, interface, page, discovery) — в Kernel (`agent/skill/`).
  Знания о сервисах и панелях — в скилах их пакетов (`skills/<name>/SKILL.md`, как сейчас у
  модулей): `variables.md` → скил панели Variables, `services.md` → скилы модулей. Плюс
  скилы источников.
- Модули дают Agent только скилы; своих инструментов у них нет — действия идут через
  общие `console_*`.
- Панель Agent — wasm-модуль `modules/panels/agent/`: чат и подтверждения запуска.
  Настройки Agent (модель, Reasoning) — в Settings, вкладка Agent.
- Данные: `<data>/.agent/` — состояние CLI; чаты и настройки — в db (сейчас localStorage).

### App (04.11)

```
app/
├── extension/          manifest.json, devtools.html/js, services.html/js
├── kernel/src/         без UI
│   ├── lib.rs          `Callback` — чем Kernel отвечает (Shell оборачивает в него свои)
│   ├── js.rs           обёртки web-sys/js-sys (wasm)
│   ├── host.rs         `Reply` — ответ Host; host/port.rs — `Transport` (wasm):
│   │                   connectNative, куски, отмена   ← browser/transport.rs
│   ├── runtime.rs      `Kernel` окна: API для Shell (`run`/`stop`/`check_changed`,
│   │                   `apply`, `reply`, `load_modules`, `document`, …), `Event` — что
│   │                   слышит Shell, трейт `Platform` (запросы к Host, таймеры, замки,
│   │                   загрузка модулей, kv/буфер/страница); исполнение эффектов модулей,
│   │                   ответы Host, список и старт модулей; runtime/runs.rs — запуски
│   │                   (← console_panel, kernel_panel), runtime/turns.rs — ходы Agent
│   │                   (← agent.rs), runtime/browser.rs — `Platform` браузера
│   ├── startup.rs      `Startup` окна (живёт дольше `Kernel`): список модулей с Host,
│   │                   база и библиотека workspace (открытие, импорт старого хранилища),
│   │                   сохранения других окон, Retry, выбранная сессия, `Event`/`View`
│   │                   для Shell, трейт `Boot`; startup/browser.rs — `Boot` браузера
│   │                   (свой `Transport`, IndexedDB, beforeunload)  ← browser/sessions.rs
│   ├── modules.rs      загрузка wasm, MVU: update(model, msg) -> (model, cmds), protobuf
│   │                   на границе, исполнение команд; wasm.rs — wasm-экземпляр
│   ├── modules/        registry.rs — модули App и их состояние (← kernel.rs; встроенные
│   │                   и связанные модули даёт Shell через `link`), loader.rs — wasm с Host,
│   │                   кеш IndexedDB, список модулей (← browser/kernel_modules.rs),
│   │                   services.rs — сервисы консоли (← services.rs)
│   ├── workspace.rs    текущее состояние: снимок и восстановление (раскладка, ширины,
│   │                   документы, ответы, сессионный kv модулей)  ← sessions.rs;
│   │                   workspace/store.rs — сохранение, очередь правок, миграция
│   │                   (← browser/sessions.rs)
│   ├── document.rs     модель документа консоли: чеки, ответы, регионы, identity,
│   │                   `check_at`, правки (document/{checks,console,identity}.rs)
│   ├── editor.rs       язык редактора: модуль сервиса + вклады панелей ({{…}} от Variables),
│   │                   подсветка, свёртки, подсказки (editor/completion.rs), шаблоны
│   │                   (editor/templates.rs); трейт `Services` — что Shell знает о модулях
│   ├── requests.rs     конвейер: чек → запрос модуля, `Run` (переменные истории, конец
│   │                   запуска: render → after_response), команды модулей, которые
│   │                   обслуживает Host, Stop  ← console.rs, kernel_panel.rs;
│   │                   requests/format.rs — текст ответа  ← format.rs
│   ├── storage.rs      порядок вкладок, kv модулей (сессия / localStorage), настройки
│   │                   Agent  ← storage.rs, kernel_panel.rs; storage/database.rs —
│   │                   IndexedDB  ← browser/session_database.rs
│   ├── locks.rs        Web Locks (темп Splunk и т. п.)  ← kernel_panel.rs
│   ├── page.rs         выполнение кода на исследуемой странице (inspectedWindow.eval)
│   │                   ← browser/app/agent.rs
│   ├── updates.rs      установка обновления App, reload, проверка второго окна
│   │                   ← browser/updates.rs
│   ├── events.rs       события модулям (Document, Hints, Discover, Workspace)  ← kernel.rs
│   ├── agent.rs        консоль, с которой работает Agent: id чеков, ревизии, правки
│   │                   ← agent_workspace.rs
│   └── activity.rs     состояние модуля и его Debug (детали, последние строки лога)
├── widgets/src/        без логики приложения и без Kernel; CSS рядом с виджетом (`CSS`),
│   │                   цвета — только var(--…) из темы Shell
│   ├── lib.rs
│   ├── dom.rs          общие обёртки web-sys/js-sys  ← browser/mod.rs
│   ├── view.rs         UI-дерево модуля → виджеты (Tabs, List, Table, PanelFrame…),
│   │                   действия → msg; view.css
│   ├── editor.rs       View + Handle  ← browser/editor/{mod,view,handle}.rs
│   ├── editor/         state, text, commands, history, layout, render, highlight (кеш;
│   │                   токены от расширения), fold (механика), brackets, model (модель
│   │                   одного редактора, ← editor/session.rs), facade, syntax, hover
│   │                   (← profiles.rs), json; extension.rs — нейтральный трейт `Extension`
│   │                   (токены, свёртки, подсказки, hints, регионы вывода, полосы строк) и
│   │                   `Languages` для редакторов из UI-дерева; о чеках не знает; editor.css.
│   │                   `Handle::mount` — отдельный редактор для table-performance
│   ├── find_bar.rs     ← find.rs, browser/find.rs (+ find_bar/view.rs)
│   ├── check_rail.rs   ← browser/check_rail.rs
│   ├── status_bar.rs, tree.rs, indicator.rs  ← views.rs, activity.rs
│   ├── resizer.rs      ← browser/pane_resizer.rs
│   └── markdown.rs     ← browser/agent_markdown.rs
├── shell/src/          → app.wasm; точка сборки: единственная часть, знающая и Kernel, и
│   │                   Widgets
│   ├── lib.rs          старт: Kernel, монтирование, связывание виджетов с Kernel
│   ├── link.rs         `link()`: модули App в Kernel — Settings, натив — все модули (тесты)
│   ├── language.rs     `Language`: реализация `Extension` и `Languages` над Kernel
│   │                   (document + editor), `Modules` — реализация `kernel::editor::Services`
│   ├── browser/        корневой компонент app.rs (+ app/console_panel, kernel_panel,
│   │                   agent): раскладка, редакторы, показ запусков и статусов по
│   │                   событиям Kernel, команды, которые обслуживает Shell;
│   │                   sessions.rs — компонент Host (показывает `Startup`: «Preparing
│   │                   sessions…», ошибку с Retry, App на сессии), mount
│   │                   (`window.__servicesMountEditor`)
│   ├── layout.css      вся раскладка (верхнее меню, рабочая область, боковая панель,
│   │                   нижняя строка); раскладка в коде — пока в browser/app
│   └── theme.css       тема: переменные цветов и шрифтов, body; services.html подключает её
│                       ссылкой (`theme.css`), чтобы до загрузки wasm не было белого экрана
```

Kernel — маршрутизатор между редактором/Agent, модулями и Host (решено 09.11). Он ведёт
весь запуск: разбор чека (`parse_run`) → `build`/`Run` модуля → `before_request` → вызов
драйвера через Host → поток и частичный вывод → `render` → `after_response`; Stop,
повторный запуск того же чека, Repeat и его таймеры, ожидание загрузки модулей, id
запусков, замки, ходы Agent и его запуски — тем же путём. API: `Kernel::run(RunRequest
{service, source, line, check, title, history, origin})` → ключ запуска, `stop(key)`,
`stop_request(id)`, `check_changed(key, source)` (правка чека заканчивает Repeat);
изменения Kernel сообщает своим `Callback<Event>` (`Run(RunView)`, `Response`, `Notice`,
`Changed`, `Command`, `AgentTool`, …), отложенно — когда сам закончил шаг. Браузер Kernel
видит через `Platform` (wasm — `runtime/browser.rs`: `setTimeout` через web-sys, Web Locks
и `Transport`), тесты — через подделку (`kernel/tests/runs.rs`). Shell только
передаёт события виджетов в Kernel и показывает его состояние: раскладка, редакторы,
вставка ответа, строка статуса и индикаторы из `RunView`; команды, которым нужен
документ или workspace Shell (`document.read`, `workspace.*`, `app.update`,
`app.reload`, `agent.approve`) и инструменты консоли Agent, Kernel передаёт Shell
событиями, Shell отвечает `reply`/`agent_result`.

Исключение из правила «CSS рядом с владельцем через `include_str!`» (решено 08.11):
`shell/src/theme.css` подключается ссылкой из `services.html` (xtask кладёт его в расширение
как `theme.css`), чтобы фон и цвета были до загрузки wasm. Остальной CSS — в wasm.

Ушли из Kernel в панели: подстановка переменных (Variables), History и маскировка
(History), инструменты Agent, `page.js` и корневой скил (Agent), библиотека сессий (Sessions).

Форматирование ответов (решено 05.11):
- `tables.rs` (сетка результатов, JSON в ячейках) → SDK `lang/table` (mysql, splunk,
  opensearch).
- `logfmt.rs` → модуль Splunk.
- `format.rs` делится: вывод ответа (включая код под Splunk) → `render` модулей,
  `history_document` → панель History, `status` → StatusBar; `duration` — в Kernel
  (`requests::format`), StatusBar получает готовый текст.

### Kernel и панели (05.11)

Kernel не знает ни одной панели: даёт точки расширения, логика — в панелях-модулях.

- Конвейер запроса с крючками: `before_request` — Variables подставляет переменные;
  `after_response` — History пишет запись, маскировка — там же.
- Подсветку и подсказки `{{…}}` поверх редактора даёт Variables как вклад в редактор.
- Agent — панель-модуль с инструментами; Kernel даёт общий API (читать и править документ,
  запустить чек, выполнить код на исследуемой странице).
- Settings пользуется общими возможностями Kernel: модули, источники, обновление.
- Kernel `workspace.rs` снимает и восстанавливает текущее состояние; панель Sessions хранит
  библиотеку сессий (список, сохранить, переключить, переименовать) через `snapshot`/`restore`.
  Модуль помечает свои ключи kv как сессионные — они входят в снимок.
- Цена: больше API (крючки, порядок, возможности), вызов wasm на каждый запрос, панели видят
  все запросы (разрешений нет).
- API модулей — общее ядро и два расширения (вариант C): в SDK `core` (манифест, загрузка,
  MVU `init`/`update`/`view`, Cmd: драйверы, хранилище, блокировки, статус; скил), `service`
  (язык, построить вызов, разобрать ответ, discovery, действия Export/Import, Options) и
  `panel` (кнопка в меню, боковая панель, крючки `before_request`/`after_response`). Вклад в
  редактор (`highlight`, `complete`, `fold`, `diagnose`) — общий блок, доступен обоим
  (Variables даёт `{{…}}`). Одно ABI и один загрузчик в Kernel; вид в манифесте
  (`service` | `panel`) определяет обязательные части. Модули общаются только через Kernel.
- Версии: одно поле `abi` в манифесте, Kernel грузит только точно совпадающее. Ломаем API
  когда нужно; стандартные модули пересобираются и выходят вместе с App. protobuf скрыт в SDK.

#### Манифест (05.11)

- Статический `manifest.json` — только то, что нужно до загрузки wasm (Settings показывает
  модуль, не запуская его). Поведение — в коде модуля через SDK.
- В манифесте нет ничего, что интерпретирует Kernel: `params` (с полями CLI), `console`
  (`sections`, `payload`, `response`, `errorBody`, `command`, `discovery`, `keywords`,
  `hints`) уходят в код модуля.
- Вид один: `service` или `panel`. Модулю нужны оба — два модуля в одном пакете.
- Требования: драйверы и их версии, минимальная версия App, ABI; Kernel проверяет до
  загрузки.
- `order` — порядок вкладки или кнопки и порядок крючков (Variables раньше остальных);
  `toolbar` (необязательно) — позиция кнопки панели в верхнем меню, если она не совпадает с
  порядком крючков (кнопка Transfer стоит на позиции 2 в том же списке). ABI 2 (09.11).
- `transfer` (необязательно, у сервиса) — команда, которой сервис экспортирует и импортирует
  чек (`curl`, `grpcurl`); без него у сервиса нет Export и Import.

```json
{ "id": "rest", "kind": "service", "name": "REST", "version": "3.7.0",
  "description": "Send a REST request and inspect the response.",
  "abi": 2, "app": ">=4.0", "drivers": {"http": "1"}, "order": 1 }
```

#### Cmd (05.11)

Запросы модуля к Kernel, все асинхронные:

| Группа | Cmd | Кому нужно |
|---|---|---|
| Драйверы | `call`, `stream`, `cancel` (http, grpc, mysql, process, tcp) | сервисы |
| Хранилище | `get`, `put`, `delete`, `watch` — kv своего модуля в db | все |
| History | `insert`, `search` | History |
| ОС | окружение (`env`), буфер обмена | Variables, все |
| Источники | сырые документы источников по порядку | Variables, Dictionary, Templates, Hints |
| Документ | прочитать, вставить, заменить, выбрать чек, запустить чек | Templates, Agent |
| Workspace | `snapshot`, `restore`, `retry` (как Retry в Shell) | Sessions |
| Страница | выполнить код на исследуемой странице | Agent |
| Agent | ход диалога, Stop, регистрация инструментов | Agent |
| Модули и App | установить, удалить, обновить; обновить App, reload | только встроенные (Settings) |
| Служебное | блокировка, таймер, свой индикатор и статус, уведомление, лог | все |

- Ответ — по метке: модуль кладёт в Cmd свой `tag`, ответ приходит `Msg::Reply { tag,
  result }`; стрим — несколько `Reply` с тем же `tag` и `End`.
- Модуль видит только свой kv; чужие данные напрямую не читает.
- Разрешений нет; исключение — установка модулей и обновление App только у встроенных.
- History — отдельные Cmd (`insert`, `search`): поиск на kv не сделать.
- Имена Cmd в SDK (`api/sdk/src/cmd.rs`): `driver.call`, `driver.stream`, `driver.cancel`;
  `vars.resolve` (`mode: "json"` — значения экранируются как содержимое JSON-строки);
  `history.insert`, `history.search` (хранилище — Host); `os.env` (окружение из Host),
  `clipboard.read`/`write` (App); `sources.read` (сырые документы источников по порядку);
  `editor.contribute`; `document.read`/`insert`/`replace`/`select`/`run`;
  `workspace.list`/`restore`/`copy`/`delete`; `page.eval`, `page.network`; `agent.turn`
  (стрим событий Copilot), `agent.stop`, `agent.reply` (ответ на `ToolCall`; ответы
  остановленному ходу не доставляются), `agent.approve`, `agent.sessions`, `agent.models`,
  `agent.options`; только встроенные: `modules.check`/`install`/`remove`,
  `sources.list`/`save`/`token`/`apply`/`open`, `app.update`, `app.reload`;
  `run.output`, `run.result` (тело, исход, язык, Debug или ошибка); `status.set` (с меткой
  запуска — статус запуска, например `3 rows`; у занятого статуса App показывает время с
  начала; необязательное `stop: {msg, testid}` — кнопка Stop в строке статуса панели,
  `cmd::status_stoppable`); `workspace.retry`; `kv.get`/`put`/`delete`/`watch` (kv модуля в `localStorage` `module:<id>:<key>`,
  сессионные ключи — в снимке сессии; ответ на `put`/`delete` сессионного ключа с меткой
  приходит после записи сессии, ошибкой — если снимок изменило другое окно); `timer.after`; `lock.acquire`/`lock.release`
  (именованный замок, общий для всех окон App — Web Locks); `notify`, `log`, `diagnostics`.
- Запуск, который ведёт модуль (решено 07.11, вариант A): модуль, подписанный на `run`,
  получает `Run { tag, check }`, сам делает сколько нужно вызовов драйвера (ответы по меткам),
  берёт замки и таймеры и заканчивает Cmd `run.result`. Stop: модуль получает
  `Cancel { tag }` (замки ещё держатся), затем Kernel отменяет его открытые вызовы и
  освобождает замки. Сервисы с одним вызовом остаются на простом пути
  `build → call → render`.

#### Msg (05.11)

| Msg | Когда |
|---|---|
| `Init { settings }` | старт модуля (`settings`: `timezoneOffset`, `instance` — id окна App) |
| `Ui { msg, value }` | действие в своём UI: клик, ввод (`value` — текст поля) |
| `Reply { tag, result }`, `End { tag }` | ответ на Cmd, куски стрима |
| `Document { check, sections }` | документ сервиса изменился |
| `Discover` | ручной discovery |
| `Hints { check, pos }` | пользователь запросил подсказки (Ctrl+Space, `/` в адресе) — ленивый discovery |
| `Run { tag, check }`, `Cancel { tag }` | Run чека модулем, который ведёт запуск сам; Stop |

- Значения переменных модуль просит Cmd `Resolve { tag, text, mode }`: App → панель
  Variables → её хранилище; ответ приходит в `Reply { tag }`. `mode: "json"` (решено 07.11):
  App экранирует значения как содержимое JSON-строки — так GraphQL подставляет переменные в
  JSON. Модуль сам экранирует под свой язык (Terminal — кавычки shell после Resolve); App
  ничего не подставляет после модуля. Kernel отвечает сам синхронными вызовами `resolve`
  панелей, которые держат переменные (Variables, Dictionary), по `order`; без таких панелей
  текст с `{{` — ошибка.
| `Request { started / finished }` | запуск и завершение запросов |
| `Workspace` | restore: сессия переключена |
| `Storage { key }` | изменился ключ из `watch` |
| `Shown`, `Hidden` | панель открыта или закрыта |
| `Modules` | модуль установлен, удалён или обновлён |
| `ToolCall { tag, name, args }` | Agent вызывает инструмент |

- Подписки как в Elm: `subscriptions(model)` возвращает нужные события; остальные модулю не
  приходят.
- Подсветка и подсказки — синхронными вызовами; `Document` — с задержкой ~300 мс и только по
  изменённому чеку (discovery, диагностика).
- UI ↔ Msg: в `view` модуль пишет `on_click: Msg::Delete(id)`, SDK кодирует его в байты,
  Kernel возвращает в `Ui`; автор работает со своим `enum Msg`.
- Закрытая панель выключена: `view` не вызывается, UI размонтирован, событий и подписок нет,
  таймеры и стримы отменяются. Работают только её синхронные крючки (`before_request`,
  `after_response`). Черновики — в своём kv; при открытии — `Shown`, панель читает их
  обратно. Закрытый App выгружает все модули и завершает процесс Host.

#### Синхронные вызовы (05.11)

Kernel вызывает модуль напрямую и сразу получает ответ:

| Блок | Вызов | Когда |
|---|---|---|
| Редактор (оба вида) | `highlight(state, lines) -> tokens, state` | ввод, прокрутка |
| | `complete(check, pos) -> items` | подсказки |
| | `fold(check) -> ranges` | свёртки |
| Сервис | `build(check) -> driver call` | Run |
| | `render(chunk) -> response text, metadata` | ответ или кусок стрима (`stream: true` — по куску) |
| | `export(check) -> text`, `import(text) -> check` | Export (чек с подставленными переменными, секреты тоже), Import |
| Панель | `before_request(call) -> call or error` | перед драйвером |
| | `after_response(call, response) -> cmds` | после ответа |
| | `resolve(text, mode) -> text, secrets` | Cmd `vars.resolve`, Run и Export сервиса |

- Метаданные ответа (решено, meta=A): `render` отдаёт кроме текста типы колонок, как их
  сообщил сервер (`text`, `number`, `boolean`, `datetime`, `json`; MySQL — из метаданных
  драйвера). App хранит их с ответом и передаёт модулю в подсветку (`highlight` — по строке
  маркера ответа) и в свёртки (`Check.columns`); `Columns-Type` в чеке — явная замена
  сообщённого типа.
- Синхронный вызов только читает свою модель (схема discovery для подсказок), не меняет её и
  не отдаёт Cmd. Исключение — `after_response` возвращает Cmd (History `insert`).
- Подсветка — построчное состояние: Kernel передаёт пачку строк с состоянием на входе и
  продолжает, пока состояние на конце не совпадёт с кешем.
- `diagnose` асинхронный: модуль получает `Document` и отвечает Cmd «диагностика».
- Бюджет времени (например, 5 мс на пачку подсветки): превышение видно в Debug модуля,
  модуль не отключается.
- `before_request` — цепочкой по `order`; ошибка останавливает запуск с сообщением
  («неизвестная переменная `{{:host}}`»).
- `{{…}}` поверх языка: Kernel сводит токены сервиса и Variables, внутри `{{…}}` — токены
  Variables.

#### UI-дерево (05.11)

- Узлы двух слоёв: контейнеры `PanelFrame`, `Tabs`, `List`, `Table`, `Row`, `Column`;
  элементы `Text`, `Markdown`, `Tree`, `Input`, `Select`, `Checkbox`, `Slider`, `Button`,
  `Indicator`, `Editor`. Своего HTML и CSS у модуля нет, только варианты вида (`primary`,
  `danger`, `muted`); не хватает виджета — добавляем в `widgets` для всех.
- После каждого `update` модуль отдаёт дерево целиком, Yew сравнивает; у элементов списков
  обязательный `key`.
- Большие списки — виртуальный `List { total, rows }`: модуль отдаёт видимое окно, при
  прокрутке приходит `Ui` с новым диапазоном.
- Поле ввода само хранит набираемый текст и сообщает `on_input` с задержкой или `on_change`
  (Enter, blur); модуль задаёт значение, только когда меняет его сам. Ещё не сообщённый
  ввод App отдаёт модулю раньше любого другого его действия в UI и до закрытия панели.
- Вкладка `Tabs` может переименовываться на месте (`on_rename`: двойной щелчок, Enter или
  blur — сохранить, Escape — отменить) и нести свет (`indicator`); `Tabs.on_reorder` —
  перетаскивание вкладок, значение `["from","to"]`.
- Общие возможности виджетов (09.11, ABI 2): у любого узла `.title()` (подсказка) и `.label()`
  (aria); `Tab`: `.closable(&msg)` (× со значением-ключом, `.confirm`, `.close_key`),
  `.plain()`, `.title()`; `Tabs`: `.on_add` (кнопка `+`, `.add_prompt`, `.add_button`);
  `TableRow.detail` (содержимое под строкой), `Table.widths` (проценты), варианты строк
  `current`/`muted`, таблицы `roomy`; `Input.size`; `Editor`: `.placeholder`, `.debounce(мс)`,
  краевой режим `is-flush` (в теле панели и вкладки); `ui::field(метка, элемент)`,
  `ui::anchor(метка, url)` (только http/https), `ui::combobox` (поиск по списку,
  `Choice.detail`), `Slider` с `.labels`, `.marker`, `.disabled`. Варианты: панель `flush`/
  `padded`/`editor`; текст `warning`/`bold`/`small`/`truncate`/`paragraph`/`pre`; кнопка `tab`/
  `inline`; `grow`; строка `nowrap`/`end`; колонка `ruled`/`loose`/`indent`/`inset`/`tight`/
  `quote`; список `dense`/`capped` (строка, ставшая `primary`, прокручивается в видимую
  область). Подробности — `examples/echo/README.md`.
- Рисует дерево App: `app/widgets/src/view.rs` (узел → виджет, действие → `Ui`).
- `Editor { language: "<id модуля>", text, on_change }` — подсветку даёт модуль с этим id
  (Templates показывает шаблон REST языком REST).
- Места для UI сервиса фиксированы: форма Options, Export и Import в панели Transfer
  (своя панель App), Debug в StatusBar. Панель — `PanelFrame` (заголовок, тело, низ).
- Заголовок `PanelFrame` — либо название (`ui::panel(title, …)`, со светом `.indicator`),
  либо свои вкладки панели (`ui::tabbed_panel(tabs, …)`: полоса вкладок в строке
  заголовка, тело активной вкладки — над телом панели); после них в той же строке —
  `.actions(…)` (происхождение, Reset, Discard, кнопки чатов). Строка заголовка стоит
  на уровне вкладок сервисов: правая колонка Shell начинается с первой строки сетки (09.11).
- Виды текста и кнопок (09.11): `ui::link(текст, &msg)` — кнопка-ссылка (пунктир, как в
  старом UI), `ui::heading` — жирный заголовок, `ui::badge` — рамка-метка (происхождение),
  `ui::spacer()` — пустая строка `Row`, прижимающая остальное к концу; `variant` — список
  через пробел (`"link danger"`, `"quote accent"`); `column` с `warning` — рамка согласия;
  `TableRow::variant("active")` — выделенная строка таблицы. Danger — только у кнопки
  подтверждения.

Папок `assets/` нет: CSS и скрипты лежат рядом с кодом владельца и подключаются через
`include_str!`. Сейчас CSS (~1770 строк) на ~46% — виджеты, ~42% — панели (Agent, Settings,
прочие), ~8% — раскладка. Стили панелей исчезают: панели строятся из виджетов, своего CSS у
них нет. `page-agent.js` (скрипт для `page_*` на исследуемой странице) → панель Agent; Kernel даёт
только выполнение кода на странице (`page.rs`).

Панели (History, Variables, Dictionary, Templates, Hints, Agent, Sessions, Settings) —
wasm-модули в `modules/panels/`, не в `app/`.

Settings — встроенная панель-модуль `modules/panels/settings/` (вкладки Modules, Sources,
Agent): API как у остальных панелей, но поставляется внутри App, отдельно не удаляется и не
обновляется — иначе сломанные модули было бы нечем чинить. Установку модулей, источники,
обновления и список моделей Agent даёт Kernel.

## Целевое дерево (предложение)

```
devtool/
├── app/                          App
│   ├── extension/                Extension                  ← adapters/extension
│   │                             (services.html/js вместо panel.html/js)
│   ├── kernel/                   Kernel: modules (загрузка wasm), workspace, document,
│   │                             editor, requests, storage, locks, page, updates, events
│   ├── widgets/                  Widgets: view (tabs, list, table, panel_frame),
│   │                             editor/, find_bar, check_rail, status_bar, tree,
│   │                             indicator, resizer, markdown; CSS рядом с виджетом
│   │                             (тема — в Shell)
│   └── shell/                    Shell → app.wasm: lib (старт, связывание), layout, theme
├── host/                         Host                       ← rust/native
│   └── src/ bridge/              native messaging, framing, chunks, router, health
│            drivers/             net, http, grpc, mysql, process, tcp
│            files/               db (sqlite: kv + History), modules, sources, updates
│            agent/               Copilot SDK, tools, skills, models
│            system.rs            окружение login-shell, буфер обмена
├── api/
│   ├── protocol/                 App ↔ Host                 ← rust/contracts
│   └── sdk/                      для модулей                ← module-api + module-ui
│       └── src/lang/             json, headers, endpoint, table — общие языки как библиотека
├── modules/                      только wasm
│   ├── services/                 rest, graphql, grpc, mysql, splunk, artemis, opensearch,
│   │   └── <name>/               terminal: manifest.json, src/ (request, highlight,
│   │                             complete, diagnose), skill/, RELEASE_NOTES.md
│   └── panels/                   history, variables, dictionary, templates, hints, agent,
│                                 sessions, settings (встроенная)├── installer/
├── tools/                        xtask, testkit, dev, scripts
├── examples/echo/
├── docs/
└── Cargo.toml                    workspace
```

Уходит: `adapters/` с CLI, dylib модулей и stabby, промежуточная `rust/`, языки из ядра
(`completion.rs`, `sql_completion.rs`, `graphql_completion.rs`, `syntax.rs`,
`editor/sql.rs`, `editor/json.rs` — в модули и SDK).

## Dev-данные (05.11)

Корневая `modules/` — установленные модули dev-хоста для CLI (`xtask modules`, `./dev`,
`SERVICES_DATA_DIR` = корень репозитория). Уходят вместе с CLI: `xtask modules`, `./dev`,
корневая `modules/`. Модули в разработке ставятся как у пользователя: `dist/` — источник в
Settings. Пакеты выпуска — `dist/modules/`, как сейчас.

## Порядок перехода (05.11)

После каждого шага сборка и тесты зелёные.

1. Перенести крейты в новое дерево без изменения поведения.
2. SDK и загрузчик wasm в Kernel.
3. REST — первый wasm-модуль, до конца.
4. Остальные сервисы.
5. Панели по одной.
6. Разделить widgets и shell, удалить монолит.
7. Обновить документацию.

## Сделано

- 09.11 (фаза 2a): общие возможности UI для возврата старого вида панелей, ABI 2 — вкладки
  с × и `+`, подсказки и aria, таблицы (ширины, детали строк, варианты), combobox, ползунок с
  метками, ссылка, поле с подписью, плейсхолдер/задержка/краевой режим редактора, варианты
  текста/колонок/списков/панелей, Stop в строке статуса панели (`status.set`/`stop`),
  Cmd `workspace.retry`, порядок кнопок меню (`toolbar` в манифесте, Transfer в одном
  списке с панелями). Не сделано: свой testid и `data-*` у узлов, `aria-current` у строк.
- 09.11: внешний вид панелей возвращён к старому (1e94d2c) без CSS в модулях: новые
  элементы `link`, `heading`, `badge`, `spacer`, многозначный `variant`, `TableRow.variant`;
  исправлены Settings (таблица Modules, шапка), Sessions (таблица), Agent (сообщения,
  согласия), Hints/Templates/Dictionary (метки), History. Не воспроизведено: закрытие и
  порядок вкладок Dictionary/Agent (поведение виджета Tabs).
- Шаг 0: CodeMirror заменён своим редактором на Yew (MVU) без смены архитектуры. В
  пре-релизе, не выпущено. Части, переведённые из CodeMirror, переписаны с нуля.
- CLI удалён вместе с `./dev`, `xtask modules` и dev-данными в корне; `SERVICES_DATA_DIR`
  обязателен.
- Шаг 1: крейты перенесены в новое дерево (`host/`, `api/protocol`, `api/module-api`,
  `api/module-ui`, `app/ui`, `app/extension`, `modules/services/`, `examples/echo`,
  `installer/`, `tools/{xtask,testkit,scripts}`); имена крейтов и поведение прежние.
  Раздел «Что есть сейчас» описывает раскладку до переноса.
- Шаг 2: `api/sdk` (манифест, protobuf событий и синхронных вызовов, Cmd как
  `{tag, name, args JSON}`, UI-дерево, формы http, `export!`), `app/kernel` (загрузка wasm,
  Init/update/view, подписки, панели закрыты — работа отменена, конвейер
  build → before_request → after_response, ловушки, бюджет 5 мс, разбиение чека по `#name`),
  драйвер http в Host (`host/src/drivers{.rs,/http.rs}`: один клиент, редиректы сам,
  тайминги, `DriverFailure`, стрим, отмена; `health` отдаёт драйверы). App пока на старых
  модулях; подключение — в шаге 3.
- Шаг 3: REST — первый wasm-модуль на SDK (`modules/services/rest`, `lang/` в SDK: json,
  headers, endpoint, curl); пакет `package.json` + `module.wasm` без платформы. Host находит
  wasm-пакеты, отдаёт их списком и `op:"wasm"`; App кеширует wasm в IndexedDB по digest и
  ведёт REST через Kernel (подсветка, подсказки, свёртки, Run, Export/Import, discovery).
  Discovery ленивый: `Hints` (Ctrl+Space, набранный `/` в адресе) и `Discover`; переменные
  модуль получает Cmd `vars.resolve`. Временные операции Host до шага 5: `vars.resolve`,
  `history.insert`. Остальные сервисы на старом пути до шага 4. Отличия: у REST нет
  Templates до шага 5 (сценарий templates на GraphQL), Debug — `{body, outcome, request,
  response}`, исход 1xx/3xx — OK, discovery не идёт при наборе JSON без известных путей,
  REST подписан на `Document` только чтобы заранее подставить переменные в адрес.
- Шаг 4: все сервисы — wasm-модули на SDK (`modules/services/{rest,graphql,opensearch,
  mysql,grpc,splunk,terminal,artemis}`), драйверы Host http, grpc, mysql, process, tcp.
  Удалены нативный путь целиком: `api/module-api`, `api/module-ui`, `ui/`-крейты модулей,
  загрузка dylib в Host, stabby, путь `ServiceUi` и старый discovery (`type:"discover"`) в
  App; `examples/echo` — модуль на SDK; xtask и testkit собирают и ставят только wasm-пакеты.
  Язык сервисов — в модулях и `api/sdk/src/lang/` (`rest/`, `json`, `headers`, `endpoint`,
  `curl`, `sql/`, `table`, `check`, `resolve`, `shell`); из App ушли `tables.rs`,
  `editor/sql.rs`, `sql_completion.rs`, части `syntax.rs`/`format.rs`, `columns.rs` в
  протоколе. `logfmt` — в Splunk. Splunk и Artemis ведут запуск сами (`Run`, вариант A):
  Splunk — замок `splunk:<origin>`, пауза 13 с между окнами, откат на старый API при 404, повтор
  429, стрим с ранней остановкой, предел 605 с; Artemis — несколько вызовов Jolokia.
  Стрим драйвера (`driver.stream`) с `render(chunk)`: Terminal (stdout и stderr вперемешку);
  MySQL отвечает одним вызовом. Статус запуска от модуля (`N rows`, `exit N · последняя строка stderr`). `#options`
  передаются модулю (MySQL `Columns-Type`); типы колонок таблиц берутся из `Columns-Type` в
  чеке, не из метаданных ответа (MySQL угадывает JSON без объявления, Splunk нет); свёртки
  JSON в вертикальных таблицах — `table::folds` модуля. Пока модули грузятся, Sessions
  заняты (`modules loading`), а Run ждёт загрузки и запускается после неё.
  Отличия: Templates у сервисов нет до шага 5 (сценарий templates сокращён), нет крючков
  запуска до шага 5; Console `Format:` и SQL-оверлей убраны, History не форматирует тело;
  авто-discovery убран (только `Hints`/`Discover`); ответ Host `modules` —
  `{wasm, failed?}`; Debug Terminal — поля выхода на верхнем уровне и `request`; Splunk
  `resultCount` — в `debug.export`; ответ на `Cancel` модуля не показывается; статус
  запуска — только по завершении; предупреждения Artemis — в исходе и Debug; пакеты без
  платформы и без codesign.

- Шаг 5, часть 1: панели Variables и History — wasm-модули в `modules/panels/`
  (`variables`, `history`), пакеты как у сервисов. Variables: `before_request` подставляет `{{…}}` во все строки вызова драйвера вне
  запуска модуля, синхронный `resolve` отвечает на Cmd `vars.resolve` и на подстановку
  источника и заголовка запуска для History; секреты — имена `token`, `password`, … и
  зависимости. History: `after_response` по записи запуска (`checkId`, `sessionId`,
  `source`, `title`, `secrets`, исход, текст) маскирует секреты и отдаёт Cmd
  `history.insert`. Удалены операции Host `vars.resolve` и прямой `history.insert` из App;
  операция Host `history insert` осталась хранилищем Cmd `history.insert`; из Host ушли
  `resolve_params`, `resolve_for_history`, `HistoryRedactor`. Templates для всех сервисов
  вернулись: секции — `check` и маркеры, которые модуль дополняет; Host не знает секций и
  проверяет только имена; сценарий templates полный (GraphQL, MySQL, Splunk). Meta=A
  (см. «Синхронные вызовы»): MySQL отдаёт типы колонок сервера, Splunk — нет (API не
  сообщает типов полей). Id панелей (`history`, `hints`, `templates`, …) больше не
  зарезервированы Host. Kernel: закрытые панели получают только Init, Shown, Reply, End,
  Storage, Workspace; ответы на Cmd, отменённые закрытием, отбрасываются; Run ждёт, пока
  панели загружаются и читают нужное крючкам.
  Отличия: повтор запуска не подставляет переменные заново (подставлены при разборе);
  ошибка переменной для сервиса без своего запуска — ошибка разбора без префикса
  «Variables could not be resolved»; тестовый счётчик discovery считает вызовы драйвера с
  меткой `discover:` (сообщение драйвера несёт `module` и `tag`).

- Шаг 5 завершён: App рисует UI-дерево SDK (`browser/view.rs`, стили виджетов —
  `view.css`), все панели — wasm-модули в `modules/panels/` со своим интерфейсом из виджетов:
  Variables (`var` сессии), Dictionary (наборы `dictionary.json`, источники, заменил
  `vars.profiles`), History, Templates, Hints, Agent (чаты, ход, инструменты страницы,
  `page-agent.js`), Sessions, Settings (встроенный, вкладки Modules, Sources, Agent;
  установка, удаление, обновление App — только ему). `resolve` идёт по всем панелям,
  держащим переменные, по `order` и повторяется, пока текст меняется: Variables отвечает за
  `var` сессии и `env` (Cmd `os.env`), Dictionary — за свои наборы; оставшаяся ссылка —
  ошибка. Закрытые панели выключены: Kernel не
  доставляет им Ui, Storage и Reply (кроме Init, Shown и ответов загрузки); работают только
  синхронные крючки. Удалены Yew-компоненты панелей App и их CSS, `vars.profiles` и операция
  Host профилей; `page-agent.js` — в `modules/panels/agent/assets/` (в доводке шага 6 —
  в `src/`).
  Отличия: History без aria- и
  debug-атрибутов; Settings всегда показывает три вкладки, модель Agent — список выбора, а
  шкалы Reasoning нет у моделей без него; у закрытой панели нет вида;
  закрытие панели Agent останавливает её ходы; поле одной строки сохраняется по blur;
  tree рисуется как JSON; `modules.update` слит с `modules.install`; hints — по сессии,
  templates — общие.

- Шаг 5, возвращённое поведение: вкладки виджета `Tabs` переименовываются на месте (Escape
  отменяет), перетаскиваются и несут свет. Agent: свет чата — идёт, ошибка, предупреждение
  (ждёт одобрения или остановлен) — гаснет, когда чат 0,5 с на экране; переименование на
  вкладке. Dictionary: наборы переименовываются и переставляются, сброс к источнику — по
  переменной или весь набор; изменения — транзакцией под замком `dictionary` (свежее
  чтение, применение, запись), устаревшая основа даёт «changed in another window», черновик
  остаётся до «Discard draft». Hints: конфликт сессии между окнами — ошибкой ответа на
  `kv.put`. Кнопка Transfer (Export и Import) всегда в меню, кнопки панелей — из
  статических манифестов, так меню не сдвигается при загрузке модулей.
  Свет кнопки Sessions показывает автосохранение App (идёт, ошибка) и при закрытой панели.

- Шаг 6: монолит `app/ui` (services-ui) разделён на `app/widgets` (services-widgets) и
  `app/shell` (services-shell, точка сборки `app.wasm`); `app/ui` и папка `assets/` удалены.
  Widgets не зависят от Kernel: язык редактора (сервис ли режим, маркеры, подсветка и
  свёртки модуля, подсказки, hints, переменные) — трейт `editor::language::Language`,
  реализация — `shell/src/language.rs`, ставится в `start()`; без неё — простой текст.
  В Widgets ушли редактор с моделью документа (`document`, `identity` ← checks.rs, `syntax`,
  `hover` ← profiles.rs, `json`), `view`, `find_bar`, `check_rail`, `status_bar` и `tree`
  (← views.rs), `indicator` (← activity.rs), `resizer`, `markdown`, `dom` (обёртки web-sys
  из browser/mod.rs); `RequestBlock`, `is_check` и `request_at` консоли — в
  `editor::document` (`check_at`), `duration` — в `status_bar` (позже — только в Kernel), `prepare_insertion` и типы
  подсказок — в `editor::language`. CSS разложен по владельцам (`<виджет>.css`,
  `include_str!`), цвета — переменные `:root` из `shell/src/theme.css`; раскладка Shell —
  `shell/src/layout.css` (из styles.css, console.css, ui.css); порядок каскада прежний (тема,
  виджеты, раскладка, view). Тесты — в крейтах-владельцах (`widgets/tests/editor`,
  `shell/tests/{core,modules}`); `xtask test ui` гоняет оба. Удалены мёртвые
  `console::replace_response`, `Highlighter::state_before`, зависимость `regex`, скрытый
  баннер `#native-host-status` в panel.html.
  Отличия от плана: модель документа и `Language` — в Widgets, а не в `kernel/document.rs`
  и `kernel/editor.rs` (Kernel пока не трогали); `layout.rs` не выделен — раскладка в
  `browser/app`; banner, overlay, tabs, list, table, form, panel_frame отдельными файлами не
  стали (баннер и оверлей — в Shell, остальное — в `view.rs`); `mount.rs` остался
  (table-performance); `theme.css` подключается ссылкой из panel.html, а не `include_str!`
  (иначе до загрузки wasm панель белая); классы `console-*` не переименованы. Модель
  документа, язык и классы исправлены доводкой ниже; `theme.css` оставлен исключением.

- Шаг 6, доводка (08.11): модель документа и язык редактора ушли из Widgets в Kernel, как
  в плане. `kernel/document.rs` (+ `document/{checks,console,identity}.rs`): чеки,
  `RequestBlock`, `check_at`, регионы ответов, identity, правки консоли; `kernel/editor.rs`
  (+ `editor/{completion,templates}.rs`): подсветка, свёртки и подсказки модулей, шаблоны,
  сохранённые hints, трейт `Services` (что Shell знает о модулях).
  Редактор Widgets о чеках не знает: нейтральный трейт `editor::extension::Extension`
  (синхронизация текста, правки, токены, свёртки, подсказки, вставка, hints, регионы
  вывода, полосы строк `editor-band-even/odd`, опции) и `Languages` для редакторов из
  UI-дерева; `Handle::with_extension`. Реализация — `shell/src/language.rs` (`Language`
  над Kernel, `Trees`, `relight`); `editor::language`, `document`, `identity`, `mount.rs` и
  `uuid` из Widgets удалены; `variables_changed` → `view::hints_changed`. Правка Agent —
  `agent_workspace::CheckEdit` (позиции Kernel), Shell переводит её в правку редактора.
  `mount.rs` → `shell/src/browser/mount.rs` поверх `Handle::mount`. Тесты модели
  документа — `kernel/tests/document/`; `xtask test ui` гоняет Kernel, Widgets и Shell.
  Классы виджетов переименованы по виджету: `editor-band-*`, `editor-fold-*`,
  `editor-document` (главный редактор), `find-bar*`, `check-rail*`, `status-bar*`
  (`status-bar-debug`, анимация `status-bar-spin`), `indicator-light` (`indicator-flow`),
  `resizer`; переменные `--editor-band-background`, `--resizer-min`, `--resizer-width`.
  `data-testid` не менялись. Классы раскладки Shell (`console-root`, `console-toolbar`,
  `console-workspace`, `console-status-row`, …) остались: они не виджетов. `page-agent.js`
  — рядом с владельцем, `modules/panels/agent/src/`; папок `assets/` больше нет.
  `theme.css` по ссылке из panel.html — записанное исключение (см. «App»).

- Шаг 7: документация приведена к архитектуре. README (модули — и сервисы, и панели,
  `dictionary.json` с `sets`), DEVELOPMENT.md (раскладка крейтов, типы тестов, правило
  `assets/`, синтаксис `{{var:x}}`/`{{<set>:x}}`), `examples/echo/README.md` — руководство
  автора модуля: сервис и панель, манифест, события и Cmd, синхронные вызовы, UI-дерево,
  драйверы, тесты, сборка пакета. README крейтов: `host` (Host без сервисов: драйверы,
  история, модули, источники; поведение Splunk — в `modules/services/splunk/README.md`),
  `app/shell`, `app/widgets`, `app/extension`, `tools/testkit`. Скил Agent
  (`host/agent-skills/**`): наборы Dictionary вместо профилей, ссылки `{{var:…}}`,
  `{{<set>:…}}`, хранение переменных в App, discovery в модуле REST. BACKLOG: снят пункт
  про stabby, обновлены открытые пункты; описания сделанных пунктов — история, не
  правились. CI: wasm-clippy и для `services-kernel`. xtask: `test behavior` гонял
  несуществующий `host --test modules` — теперь тесты модулей (`--module ID` — один
  модуль), драйверы и history Host.

- Доводка после шага 7 (08.11):
  - Страница App `panel.html`/`panel.js` → `services.html`/`services.js` (расширение,
    `devtools.js`, xtask, тесты, документация); старое имя не поддерживается.
  - Dictionary: «профиль» → «набор» (set) в коде, сообщениях об ошибках, подписях
    подсказок (`Dictionary set X`, `Search Variables, Dictionary sets and env`; у `{{var:`
    и `{{env:` — `Variables` и `Environment`), фикстурах тестов; `dictionary.json` пишется
    с `sets`, старый `profiles` по-прежнему читается (Host и панель Dictionary).
  - Остаток Kernel перенесён из Shell (см. дерево «App»): `host.rs` (← transport),
    `modules/{registry,loader,services}.rs` (← kernel.rs, kernel_modules.rs, services.rs),
    `workspace.rs` + `workspace/store.rs` (← sessions.rs, browser/sessions.rs без
    компонента: открытие с миграцией `localStorage` и импорт `servicesConsole.state` —
    `store::open`/`bootstrap`), `storage.rs` + `storage/database.rs` (← storage.rs,
    session_database.rs; kv модулей, настройки Agent, id окна Agent — из kernel_panel/agent),
    `requests.rs` + `requests/format.rs` (← console.rs, format.rs; из kernel_panel —
    `Run` с разрешением переменных истории и концом запуска, `Finish`, `STOPPED`, `strip`,
    команды модулей, которые обслуживает Host — `hosted`/`After`), `locks.rs`, `page.rs`
    (← browser/app/agent.rs), `updates.rs` (корень окна App — параметр, Shell передаёт
    `.console-root`), `events.rs` (Document, Hints, Discover, Workspace, рассылка),
    `activity.rs` (статус модуля и его Debug), `agent.rs` (← agent_workspace.rs).
    Kernel не знает Yew: свой `Callback`, Shell оборачивает свои (`browser::on`). Kernel не
    знает модулей App: Shell передаёт их `services_shell::link()` (Settings; натив — все
    модули), тесты Shell вызывают его первым. Текст строки статуса (`status_text`) остался
    в Shell: в нём длительность из `status_bar`. В Shell остались запуски и их таймеры,
    редакторы и показ статусов (состояние компонента App) и компонент Host (запуски и
    строка статуса потом ушли в Kernel — см. «Запуски в Kernel»). Тест
    форматирования — `kernel/tests/format.rs`; тесты workspace и Agent остались в Shell:
    им нужны связанные модули. Из Shell удалены зависимости `base64`,
    `serde-wasm-bindgen` и IndexedDB-возможности `web-sys`.
  - CI: `cargo clippy --workspace --all-targets -- -D warnings` перед тестами.

- Уборка остатков (08.11):
  - `installer/tests/packaged.rs` переведён на текущий протокол: Host отвечает за
    `health`, источники, установку модулей (`<id>-<version>.module.tgz`) и их список после
    перезапуска, `env` (окружение оболочки), драйверы `http` и `process`; сохранность
    данных при обновлении и удалении проверяется по `.sources.json` (Variables живут
    в App). Запросы `rest`/`vars`/`terminal`/`discover` к Host и OpenAPI-фикстура удалены.
  - Удалён мёртвый код: `host/src/core/operations.rs` и поле `Runtime::operations`,
    никогда не заполняемые `Error::duration_ms`/`Error::history` и `Error::with_debug`,
    `services_contracts::ParamConfig`, `Reply::into_value` (testkit).
  - Неиспользуемые зависимости (`cargo machete --with-metadata`): `serde` в Shell,
    `wasm-bindgen-futures` в Widgets, `prost` (dev) в mysql и opensearch.
  - Удалены устаревшие сборка `app/extension/dist/` (со старыми `panel.*`; собирается
    заново), пустые `app/extension/test/` и `.distribution-cache/` (и его строка
    в `.gitignore`); заголовки тестов Host обновлены (нет CLI и «core вокруг модулей»).
  - Оставлены строки `.gitignore` старых данных (`.variable-profiles.json`,
    `.dictionary-profiles.json`, `.console-hints.txt` и т. п.): такие файлы с данными
    пользователя лежат в корне рабочих копий, без строк они попадут в `git status`.

- Запуски в Kernel (09.11): прежняя запись «в Shell остаются запуски, их таймеры и ответы
  Host (08.11)» не была решением и заменена (см. «App»).
  - `kernel/src/runtime.rs` — `Kernel` окна (`Kernel::new(on_event, platform)`, в браузере
    `Kernel::browser`), `Event`, `Platform`; исполнение эффектов модулей (вызовы
    драйверов и стримы, таймеры, замки, команды, которые обслуживает Host, вклады в
    редактор), список и старт модулей, Document по паузе в правке. `runtime/runs.rs` —
    запуски (`RunRequest`, `Source::{Check, Approved}`, `Origin::{Console, Agent}`,
    `RunView`, `Repeating`): разбор, конвейер `requests::Run`, вывод, Stop, повтор,
    Repeat с коммитом History, ожидание модулей, остановка при скрытии. `runtime/turns.rs`
    — ходы Agent (стрим, инструменты, подтверждения, Stop). `runtime/browser.rs` —
    `Platform` браузера. `host.rs` теперь только `Reply`; `Transport` — `host/port.rs`.
    `requests::format::duration` и строка статуса запуска — в Kernel.
  - Shell (`browser/app.rs`, `app/{console_panel,kernel_panel,agent}.rs`) переводит
    события виджетов в вызовы Kernel и `Event` — в сообщение `Msg::Kernel`: показ
    запуска (`RunView` + якорь в документе), вставка ответа, статусы, ответы на
    `Event::Command`/`AgentTool`. Из Shell удалены `execute`, `send_run`, `receive`,
    `repeat_*`, `client_error`, `status_text`, сообщения ответов Host, `agent_turns`,
    `run_serial`, `transport`.
  - Тесты: `kernel/tests/runs.rs` (поддельный `Platform`, модули теста): запуск драйвера
    с `after_response`, ошибка разбора, Stop, повторный запуск, Repeat и его коммит,
    правка повторяемого чека, запуск модулем с замком и выводом, его Stop, ожидание
    старта панелей, скрытие консоли с запуском Agent. Тестов запусков в Shell не было.
  - Отличия поведения: повторный запуск узнаётся по id чека (без id — по строке), а не по
    строке якоря; при ожидании модуля документ сохраняется сразу, а ждёт сам
    `RunRequest` (раньше Shell повторял `ConsoleRun`); команды, которые обслуживает Shell,
    отвечаются асинхронно (событием); подсветка после старта модулей — после их первых
    эффектов; заголовок запуска с ошибкой разбора — строка заголовка чека (было пусто);
    исчезнувший якорь повторяемого запуска заканчивает Repeat как «Check edited»;
    установка обновления App и импорт из буфера (натив) идут через
    `Kernel::host_request`; удалена мёртвая проверка `console:{service}` в
    `console_edit_check`. `duration` был и в Kernel (`requests::format`), и в
    `status_bar` виджетов (дубль убран в «Старт окна в Kernel»).
  - `Transport` сообщает (`Event::Busy`) о каждом изменении запросов в полёте, пока связь
    занята, а не только о переходе занята/свободна: ответы Host, которые Kernel обрабатывает
    сам, больше не перерисовывают Shell, и без этого `data-busy-reason` устаревал
    (сценарий sessions).
- Старт окна в Kernel (09.11):
  - `kernel/src/startup.rs` — `Startup` (← Shell `browser/sessions.rs`): запрос
    `{"service":"modules"}` и `loader::accept` (ошибка — `console.warn`), ожидание списка
    модулей, открытие базы и библиотеки, первая библиотека из старого хранилища
    (`bootstrap`), сохранения других окон (`refresh` + `storage_epoch`), Retry (новое
    соединение с нативным Host и повторное открытие), выбранная сессия; новая библиотека
    сохраняет временную сессию 0, неудачная — прежнюю библиотеку с ошибкой. API:
    `Startup::new(boot, restored, on_event)` (в браузере `Startup::browser`), `view()`,
    `retry()`, `select(id)`; `Event::{Loading, Modules, Ready, Failed, Storage, Active}`.
    Браузер — трейт `Boot`, `startup/browser.rs`: свой `Transport`, `store::open`/
    `bootstrap`, `Store`, флаг несохранённых правок и beforeunload,
    `loader::restore()` (кеш списка модулей; раньше — в Shell `start()`).
  - Shell `browser/sessions.rs` только показывает `Startup::view()` (баннеры, App с
    ключом `"{active}:{epoch}"`) и передаёт Retry и выбор сессии; `HostProps` удалён;
    в Shell нет `Transport`, `store::open`/`bootstrap` и beforeunload (фича web-sys
    `BeforeUnloadEvent` перешла в Kernel).
  - `duration` — только в Kernel (`requests::format`); `status_bar` виджетов его больше
    не экспортирует, тест длительностей — `kernel/tests/format.rs`.
  - Отличия: не `Kernel::start`, а отдельный `Startup` — `Kernel` создаётся на каждый App
    (App пересоздаётся при смене сессии и новой библиотеке), а старт живёт всё окно;
    импорт библиотеки после открытия базы идёт сразу, а не следующим сообщением Yew.
  - Тесты: `kernel/tests/startup.rs` (поддельный `Boot`): кеш списка модулей, ожидание
    списка, плохой список и Host без списка, `bootstrap`, ошибка и Retry (без двойного
    открытия), временная сессия и ошибка импорта, сохранения других окон, выбор сессии.
- Заголовок боковой панели (09.11):
  - Регресс после перевода панелей в wasm: правая колонка начиналась со второй строки
    сетки (`grid-row: 2 / 4`), заголовок панели стоял ниже вкладок сервисов, а панели со
    своими вкладками показывали лишний заголовок («Settings» над Modules|Sources|Agent).
    До wasm (1e94d2c) заголовок панели жил в верхней строке рядом с вкладками сервисов.
  - Shell (`layout.css`): `.console-right-pane` — `grid-row: 1 / 4`, верхняя строка
    (`.console-top-bar`) — только над редактором (`grid-column: 2`, без subgrid), у
    `.console-tool-panel` нет верхней рамки — нижняя граница заголовка панели на одной
    линии с верхней границей редактора.
  - SDK: `PanelFrame.header` — oneof `Title` | `Tabs` (`ui::panel` / `ui::tabbed_panel`),
    `PanelFrame.indicator` (свет названия), `PanelFrame.actions` (`Node::actions`).
    Виджет (`view.rs`) рисует вкладки заголовка в `.ui-panel-title` (`.ui-panel-tabs`,
    те же `data-testid`, переименование и перетаскивание), без отдельной вкладки названия.
  - Вкладки в заголовке: Settings, Variables, Dictionary (когда есть наборы), Templates
    (происхождение и Discard — в `actions`), Agent (чаты; «#», «+», Close chat — в
    `actions`; при открытом списке чатов — название «Agent»). Название: Hints (свет —
    уровень последнего статуса, происхождение и Reset — в `actions`), Sessions (свет),
    History.
  - Отличия от старого вида: у вкладок Settings нет своих огней; «#» у Agent стоит после
    вкладок, а не перед ними; поле нового набора Dictionary осталось внизу.
  - Колонка (`ui::column`) в теле панели больше не сжимается меньше своего содержимого
    (раньше текст наползал на следующие узлы, и поле ввода Agent перекрывало кнопку Deny);
    вариант `primary` — колонка, которая заполняет остаток и прокручивается (переписка
    Agent, как до wasm).
  - Тесты: `browser-layout.js` проверяет, что вкладки Settings — заголовок панели без
    названия, на уровне вкладок сервисов; `browser-console-checks.js` — заголовок правой
    панели в строке вкладок сервисов (старая проверка искала удалённый
    `.console-panel-heading`), ширину панели сравнивает с колонкой редактора.
- Загрузка модулей (09.11): модули грузятся параллельно, сервис открытой вкладки первым;
  Kernel узнаёт о каждом запущенном модуле сразу (`Started`), поэтому Export/Import и
  язык сервиса становятся доступны, не дожидаясь остальных. Текущий чек уходит сервису
  (`DocumentDue`) только когда запущены он и все панели — иначе переменные ещё некому
  подставить; `loading` снимается последним `Started { done }`.
  Окно не ждёт Host (его старт с окружением shell — 1–2 с): модули из закешированного
  списка стартуют сразу с закешированными драйверами (`servicesConsole.drivers`), а ответ Host
  со списком догружает только изменившееся; модуль, который уже стартует, второй раз не
  грузится (`registry::settled`).
- Transfer (09.11): Export и Import — одна кнопка Transfer в меню и своя панель App справа
  (`kernel_panel.rs::view_transfer`): «Export curl» копирует чек командой и показывает её в
  редакторе (можно поправить и скопировать снова, Copy), «Import curl» берёт команду из буфера.
  Название команды — `transfer` в манифесте сервиса (REST и GraphQL — `curl`, gRPC —
  `grpcurl`); без него панель говорит, что сервису нечего экспортировать. Оверлей Export убран.
  Итог последнего Export или Import — в строке статуса панели, у каждого сервиса свой
  («Exported and copied», «Copied», «Imported», «Export failed: …»); ошибка зажигает кнопку
  Transfer в меню (у кнопок внутри панели света нет), в верхний баннер не идёт.

## Решить

Открытых решений нет.
