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
│   ├── lib.rs
│   ├── host.rs         связь с Host: connectNative, куски, отмена   ← browser/transport.rs
│   ├── modules.rs      загрузка wasm, MVU: update(model, msg) -> (model, cmds), protobuf
│   │                   на границе, исполнение команд  ← browser/module_ui.rs, services.rs
│   ├── workspace.rs    текущее состояние: снимок и восстановление (раскладка, ширины,
│   │                   документы, ответы, сессионный kv модулей)  ← sessions.rs (Snapshot),
│   │                   browser/sessions.rs; библиотека сессий — в панели Sessions
│   ├── document.rs     Check, Section, Options, Response, учёт чеков в редакторе
│   │                   ← document.rs, checks.rs, editor/checks.rs, часть editor/session.rs
│   ├── editor.rs       язык редактора: модуль сервиса + вклады панелей ({{…}} от Variables)
│   ├── requests.rs     конвейер before_request → драйвер → after_response, стрим, Stop,
│   │                   Repeat  ← из browser/app.rs
│   ├── storage.rs      kv через db Host  ← storage.rs, browser/session_database.rs
│   ├── locks.rs        Web Locks (темп Splunk и т. п.)
│   ├── page.rs         выполнение кода на исследуемой странице (inspectedWindow.eval)
│   ├── updates.rs      установка обновления App, reload, проверка второго окна
│   │                   ← browser/updates.rs
│   ├── events.rs       события модулям
│   └── activity.rs     состояния и индикаторы
├── widgets/src/        без логики приложения; CSS рядом с виджетом, цвета — только
│   │                   var(--…) из темы Shell
│   ├── lib.rs
│   ├── view.rs         UI-дерево модуля → виджеты, действия → msg
│   ├── editor.rs       View + Handle  ← browser/editor/{mod,view,handle}.rs
│   ├── editor/         state, text, commands, history, layout, render, highlight (кеш;
│   │                   токены от модуля), fold (механика), brackets, model (модель одного
│   │                   редактора, ← editor/session.rs), facade; editor.css. Уходят:
│   │                   json.rs, sql.rs → модули, mount.rs
│   ├── find_bar.rs     ← find.rs, browser/find.rs
│   ├── check_rail.rs, status_bar.rs, tree.rs, indicator.rs  ← browser/check_rail.rs, views.rs
│   ├── resizer.rs      ← browser/pane_resizer.rs
│   ├── markdown.rs     ← browser/agent_markdown.rs
│   ├── banner.rs, overlay.rs  уведомление сверху и экран поверх окна (статус Host, ошибки,
│   │                   обновление); когда показывать — решает Shell
│   └── tabs.rs, list.rs, table.rs, form.rs, panel_frame.rs (новые)
├── shell/src/          → app.wasm; точка сборки: единственная часть, знающая и Kernel, и
│   │                   Widgets
│   ├── lib.rs          старт: Kernel, монтирование, связывание виджетов с Kernel
│   ├── layout.rs       + layout.css: вся раскладка (верхнее меню, рабочая область, боковая
│   │                   панель, нижняя строка) — одна CSS-сетка
│   └── theme.css       тема: переменные цветов и шрифтов, body
```

Ушли из Kernel в панели: подстановка переменных (Variables), History и маскировка
(History), инструменты Agent, `page.js` и корневой скил (Agent), библиотека сессий (Sessions).

Форматирование ответов (решено 05.11):
- `tables.rs` (сетка результатов, JSON в ячейках) → SDK `lang/table` (mysql, splunk,
  opensearch).
- `logfmt.rs` → модуль Splunk.
- `format.rs` делится: вывод ответа (включая код под Splunk) → `render` модулей,
  `history_document` → панель History, `duration`/`status` → StatusBar.

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
- `order` — порядок вкладки или кнопки и порядок крючков (Variables раньше остальных).

```json
{ "id": "rest", "kind": "service", "name": "REST", "version": "3.7.0",
  "description": "Send a REST request and inspect the response.",
  "abi": 1, "app": ">=4.0", "drivers": {"http": "1"}, "order": 1 }
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
| Workspace | `snapshot`, `restore` | Sessions |
| Страница | выполнить код на исследуемой странице | Agent |
| Agent | ход диалога, Stop, регистрация инструментов | Agent |
| Модули и App | установить, удалить, обновить; обновить App, reload | только встроенные (Settings) |
| Служебное | блокировка, таймер, свой индикатор и статус, уведомление, лог | все |

- Ответ — по метке: модуль кладёт в Cmd свой `tag`, ответ приходит `Msg::Reply { tag,
  result }`; стрим — несколько `Reply` с тем же `tag` и `End`.
- Модуль видит только свой kv; чужие данные напрямую не читает.
- Разрешений нет; исключение — установка модулей и обновление App только у встроенных.
- History — отдельные Cmd (`insert`, `search`): поиск на kv не сделать.

#### Msg (05.11)

| Msg | Когда |
|---|---|
| `Init { settings }` | старт модуля |
| `Ui { msg, value }` | действие в своём UI: клик, ввод (`value` — текст поля) |
| `Reply { tag, result }`, `End { tag }` | ответ на Cmd, куски стрима |
| `Document { check, sections }` | документ сервиса изменился |
| `Discover` | ручной discovery |
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
| | `render(chunk) -> response text` | ответ или кусок стрима |
| | `export(check) -> text`, `import(text) -> check` | Export, Import |
| Панель | `before_request(call) -> call or error` | перед драйвером |
| | `after_response(call, response) -> cmds` | после ответа |

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
  (Enter, blur); модуль задаёт значение, только когда меняет его сам.
- `Editor { language: "<id модуля>", text, on_change }` — подсветку даёт модуль с этим id
  (Templates показывает шаблон REST языком REST).
- Места для UI сервиса фиксированы: форма Options, действия в верхнем меню (Export,
  Import), Debug в StatusBar. Панель — `PanelFrame` (заголовок, тело, низ).

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
│   ├── widgets/                  Widgets: view, editor/, find_bar, check_rail, status_bar,
│   │                             tree, resizer, markdown, tabs, list, form, panel_frame;
│   │                             CSS рядом с виджетом (тема — в Shell)
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

- Шаг 0: CodeMirror заменён своим редактором на Yew (MVU) без смены архитектуры. В
  пре-релизе, не выпущено. Части, переведённые из CodeMirror, переписаны с нуля.
- CLI удалён вместе с `./dev`, `xtask modules` и dev-данными в корне; `SERVICES_DATA_DIR`
  обязателен.
- Шаг 1: крейты перенесены в новое дерево (`host/`, `api/protocol`, `api/module-api`,
  `api/module-ui`, `app/ui`, `app/extension`, `modules/services/`, `examples/echo`,
  `installer/`, `tools/{xtask,testkit,scripts}`); имена крейтов и поведение прежние.
  Раздел «Что есть сейчас» описывает раскладку до переноса.

## Решить

- Пока ничего.
