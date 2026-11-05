# Рефакторинг: новая архитектура

Ядро без языков, модули — только wasm, Host — технический прокси. Обсуждено после 3.7.0
(до переписывания редактора) и 04.11. Пункт 33 в `BACKLOG.md` ссылается сюда.

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
  - Banner, Update overlay.
  - Документ: Check, Section, Options, Response.
- Префикс `console` (наследие Active Console) убрать везде: `ConsolePanel` → `SidePanel`,
  `console_panel.rs` → `workspace.rs` и т. д.
- Variables и Dictionary — разные панели и визуально, и в коде: два отдельных компонента
  вместо `ConsoleVariables` с флагом `dictionary`.
- Страница App `panel.html`/`panel.js` → `services.html`/`services.js`, как вкладка
  «Services» в DevTools. Адрес отдельной вкладки
  `chrome-extension://<id>/panel.html` есть в README и закладках, поэтому `panel.html`
  на одну-две версии остаётся перенаправлением на `services.html`. ID расширения не меняется.
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
- Переменные (`{{env:X}}`, `{{profile:x}}`, `{{:x}}`) подставляет Kernel в App перед вызовом
  драйвера; Host получает готовый запрос. Окружение для `env` Kernel берёт у Host
  (`system.rs`). Маскировка значений в истории (сейчас `history.rs` `redactor`) — тоже в App.
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

Открыто по Host: уровень grpc (транспорт байтами, JSON ↔ protobuf и `.proto` в модуле — или
всё в Host).

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
- Панель Agent — wasm-модуль `modules/panels/agent/`: чат, настройки (модель, Reasoning),
  подтверждения запуска.
- Данные: `<data>/.agent/` — состояние CLI; чаты и настройки — в db (сейчас localStorage).

### App (04.11)

```
app/
├── extension/          manifest, background, devtools, services.html/js, мост к Host,
│                       page-скрипты для page_*
├── kernel/src/         без UI
│   ├── modules.rs      загрузка wasm-модулей, MVU: update(model, msg) -> (model, cmds),
│   │                   protobuf на границе, исполнение команд
│   ├── requests.rs     вызовы драйверов Host, стрим, отмена
│   ├── variables.rs    подстановка {{…}} (Variables → Dictionary → Environment), подсветка
│   │                   поверх модуля
│   ├── redact.rs       маскировка (история, Agent)
│   ├── sessions.rs     вкладки, чеки, документ
│   ├── storage.rs      kv через db Host (вместо localStorage/IndexedDB)
│   ├── locks.rs        Web Locks (темп Splunk и т. п.)
│   ├── discovery.rs    кеш discovery — только в памяти
│   ├── events.rs
│   ├── agent.rs        список инструментов, приём вызовов
│   └── agent/          console.rs, page.rs, redact.rs, skill/
├── widgets/src/        editor/, find_bar, check_rail, status_bar, tree, resizer,
│                       markdown, tabs, list, form, panel_frame
├── shell/src/          → app.wasm: top_bar, workspace, side_panel, banner,
│                       update_overlay, settings/ (modules, sources, agent)
└── assets/             css
```

Панели (History, Variables, Dictionary, Templates, Hints, Agent, Sessions) — wasm-модули в
`modules/panels/`, не в `app/`.

## Целевое дерево (предложение)

```
devtool/
├── app/                          App
│   ├── extension/                Extension                  ← adapters/extension
│   │                             (services.html/js вместо panel.html/js)
│   ├── kernel/                   Kernel: sessions, checks, document, requests,
│   │                             modules (загрузка wasm), events, storage, format, tables
│   ├── widgets/                  Widgets: editor/, find_bar, check_rail, status_bar, tree,
│   │                             resizer, markdown, tabs, list, form, panel_frame
│   ├── shell/                    Shell → app.wasm: top_bar, workspace, side_panel, banner,
│   │                             update_overlay, settings/ (modules, sources, agent)
│   └── assets/                   css
├── host/                         Host                       ← rust/native
│   └── src/ bridge/              native messaging, framing, chunks, router, health
│            drivers/             net, http, grpc, mysql, process, tcp
│            files/               db (sqlite: kv + History), modules, sources, updates
│            agent/               Copilot SDK, tools, skills, models
│            system.rs            окружение login-shell, буфер обмена
├── api/
│   ├── protocol/                 App ↔ Host                 ← rust/contracts
│   └── sdk/                      для модулей                ← module-api + module-ui
│       └── src/lang/             json, headers, endpoint — общие языки как библиотека
├── modules/                      только wasm
│   ├── services/                 rest, graphql, grpc, mysql, splunk, artemis, opensearch,
│   │   └── <name>/               terminal: manifest.json, src/ (request, highlight,
│   │                             complete, diagnose), skill/, RELEASE_NOTES.md
│   └── panels/                   history, variables, dictionary, templates, hints, agent,
│                                 sessions
├── installer/
├── tools/                        xtask, testkit, dev, scripts
├── examples/echo/
├── docs/
└── Cargo.toml                    workspace
```

Уходит: `adapters/` с CLI, dylib модулей и stabby, промежуточная `rust/`, языки из ядра
(`completion.rs`, `sql_completion.rs`, `graphql_completion.rs`, `syntax.rs`,
`editor/sql.rs`, `editor/json.rs` — в модули и SDK).

## Сделано

- Шаг 0: CodeMirror заменён своим редактором на Yew (MVU) без смены архитектуры. В
  пре-релизе, не выпущено. Части, переведённые из CodeMirror, переписаны с нуля.

## Решить

- Settings — в Shell (панель управляет модулями, поэтому сама не модуль)?
- Куда деть собранные пакеты из корневой `modules/`: `dist/` или только ассеты релиза.
- Контракт Kernel ↔ модуль: события и команды, типы (protobuf), версия ABI.
- Подсветка из wasm-модуля без потери скорости (пачкой строк, состояние строки между
  вызовами).
- Где хранить переменные и историю.
- Переименование ключей хранилища (`servicesConsole.*`) без потери данных пользователей.
- Порядок перехода и обновление документации.
