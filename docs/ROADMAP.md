# stepan_verify — roadmap

Нативный (Rust + gpui-kit) центр управления агентами: дашборд по всем проектам,
сессии Claude Code и Codex внутри приложения, задачи/скиллы/доки в `~/.stepan_verify`.

## Архитектура

```
crates/
  stepan_verify_core    ~/.stepan_verify (skills, tasks, docs, automations, state), индекс сессий с диска
  stepan_verify_agents  провайдеры: Codex (app-server JSON-RPC), Claude (stream-json control protocol)
  stepan_verify_app     UI на gpui-kit: дашборд, сессия-чат, задачи, документы, скиллы
```

Без сервера и вебсокетов (в отличие от t3code): UI и провайдеры в одном процессе,
провайдеры — дочерние процессы `codex` / `claude`, общение JSONL по stdio.

## Как ведём агентов (по разбору t3code)

**Codex** — `codex app-server -c thread_unload_delay_secs=0`, один процесс на все треды.
JSON-RPC построчно без поля `jsonrpc`: `initialize` → `initialized` → `thread/start {cwd, model}`
→ `turn/start {threadId, input:[{type:text}], approvalPolicy, sandboxPolicy}`.
События: `item/started|completed`, `item/agentMessage/delta`, `item/reasoning/*Delta`,
`item/commandExecution/outputDelta`, `turn/completed`. Апрувы приходят как запросы
`item/commandExecution/requestApproval` / `item/fileChange/requestApproval` → ответ `{decision}`.
Продолжить: `thread/resume {threadId}`. Прервать: `turn/interrupt`.

**Claude Code** — процесс на тред:
`claude -p --output-format stream-json --input-format stream-json --verbose --include-partial-messages --permission-prompt-tool stdio --permission-mode <m> (--session-id <uuid> | --resume <id>)`.
Пишем `{"type":"user","message":{...}}` в stdin, читаем `system/init`, `stream_event`
(дельты), `assistant`/`user` (tool_use/tool_result), `result` (конец хода).
Апрувы — `control_request can_use_tool` → `control_response allow/deny`. Протокол
недокументирован: фиксируем версию CLI и пишем тесты на записанных транскриптах.
Запасной вариант — Node-sidecar с `@anthropic-ai/claude-agent-sdk`.

Режимы: approval-required / auto-accept-edits / auto / full-access →
маппинг в `approvalPolicy+sandboxPolicy` (Codex) и `permissionMode` (Claude).

## Этапы

- [x] **0. Каркас** — workspace, gpui-kit, сайдбар, `~/.stepan_verify`, индекс сессий Claude+Codex с диска
- [x] **0. Дашборд** — сессии по проектам, счётчики, «Продолжить» (пока — в Terminal.app)
- [x] **1. Провайдеры** — `stepan_verify_agents`: Claude stream-json + Codex app-server, общий `AgentEvent`, история сессий
- [x] **2–3. Экран сессии** — «Продолжить» открывает сессию в приложении: история, стриминг, карточки команд/правок, подтверждения, Стоп
- [x] **0. Документы** — нативный рендер `.md` и `.html` из `~/.stepan_verify/docs`
- [x] **4. Живой статус на дашборде** — блок «Сейчас», «активна» у внешних сессий, тосты «закончил / ждёт подтверждения»
- [x] **4a. Новая сессия** — диалог: проект (недавние + выбор папки), Claude/Codex, первая задача; CLI `--start/--cwd/--send`
- [x] **4b. Лента** — группировка подряд идущих инструментов, встроенные диффы правок (+/−, «ещё N строк»)
- [x] **4c. Всё видно** — скелетоны/лоадеры, картинки (вставки, скриншоты инструментов, MCP, Codex imageView/генерация), файлы через @, сообщения во время хода, thinking, ошибки хуков, SendUserFile
- [x] **5. Доска задач (по мотивам Vercel Board)** — колонки, claim с арендой, зависимости, журнал `state/events.jsonl`, CLI `stepan_verify task …` для агентов, «▶ Claude/Codex» из карточки, автосдача на ревью, автопилот (до 2 агентов)
- [x] **5a. Настройки** — `config.toml` + экран: режим агентов (по умолчанию полный доступ), агент по умолчанию, лимит автопилота, модель отчёта
- [x] **5b. «Изменения»** — кнопка в сессии запускает сабагента (Claude, модель из настроек): флоу-схема, добавлено/изменено/удалено, структура было→стало, UI было/стало, риски, что проверить; кэш в `state/changes/<session>`
- [x] **5c. Живые карточки на дашборде** — шаги плана/действий, текущий шаг, «ждёт ответа/подтверждения», шаг N из M, пауза; включая сессии вне stepan_verify (по хвосту файла)
- [x] **5d. Превью файлов справа** (HTML в WKWebView, код с подсветкой, md, картинки, папки) и **вложения** (скриншот области, вставка, drag&drop; в сессии, новой сессии и задачах)
- [x] **5e. Память** — `~/.stepan_verify/memory/log.jsonl` (append-only: add/edit/remove), отдельно от документов: поправки и запреты пользователя, его предпочтения, фактура проекта. Подмешивается первой в старт каждой сессии Claude/Codex (общая + проекта, с лимитами), агенты пишут сами через `stepan_verify memory add/edit/rm/search`, «Итоги сессии» добирают пропущенное. Экран «Память»: по областям и типам, цитата, автор, правка/удаление, журнал. Снимки `USER.md` и `projects/<p>.md` — для сессий вне Stepan
- [ ] **5f. Память: dreaming** — ночная автоматизация склеивает дубли, убирает устаревшее, переносит частое из проекта в общее; импорт `~/.claude/projects/*/memory` и `~/.codex/memories`
- [ ] **6. Скиллы** — `~/.stepan_verify/skills` подключаются в каждую сессию (Claude: `--add-dir`/плагин, Codex: `skill` input)
- [ ] **7. Проактивность** — `~/.stepan_verify/automations`: расписание/наблюдатели запускают агентов, результат — в задачи и доки, уведомление
- [ ] **7a. Telegram** — канал на телефон: «ждёт подтверждения / ответа» с кнопками, утренний брифинг, короткие команды
- [ ] **7b. Инструменты** — экран MCP: какие серверы видит Claude (`~/.claude.json`, `.mcp.json` проекта, коннекторы) и Codex (`~/.codex/config.toml`), где расходятся; включить сервер обоим; политики на подтверждениях
- [ ] **8. Git** — worktree на задачу, чекпоинты и диффы

## Проактивность (по разбору always-on ботов, Sarah / Composio)

Always-on бот = harness (у нас Claude/Codex) + инструменты + каналы + память + триггеры + права.
Как это ложится на stepan_verify:

| Слой | В stepan_verify |
|---|---|
| Harness | Claude Code / Codex как есть, stepan_verify только оркестрирует |
| Инструменты | терминал агента + MCP/CLI (API лучше браузера); Composio/MCP для внешних сервисов |
| Каналы | само приложение (дашборд, уведомления macOS); позже Telegram/iMessage для «бот в списке чатов» |
| Память | файлы в `~/.stepan_verify/memory/`: `USER.md` (предпочтения), `MEMORY.md` (долгие факты), `projects/<name>.md`, дневные заметки `daily/YYYY-MM-DD.md` — подмешиваются в старт сессии |
| Dreaming | ночная автоматизация: читает дневные заметки и итоги сессий, переносит устойчивое в `MEMORY.md`, склеивает дубли |
| Триггеры | `~/.stepan_verify/automations/*.md`: расписание (heartbeat, «каждое утро сводка по проектам») и события (новый PR/ревью, упавший CI, тикет, файл в папке) |
| Права | у каждой автоматизации свой режим (read-only / auto-edits / full) и рабочая папка; опасное — только через подтверждение в приложении |

Первая автоматизация, которую стоит сделать: **утренний брифинг** — агент проходит по
активным проектам (git log, открытые PR, незакрытые задачи, сессии со вчера), пишет
`docs/brief-YYYY-MM-DD.md` и ставит задачи во «Входящие».

## Идеи из Vercel Board (эксперимент Vercel, ноябрь 2026)

Board — это не канбан, а общая «шина» для людей и агентов: задача = тред с состоянием,
агенты её *забирают* (claim с арендой/lease), пишут ход работы в тред, *сдают* (complete со ссылкой на PR),
зависимые задачи открываются, когда готовы предыдущие, события будят агентов (watchers), агент может
сам себе поставить напоминание (schedule). Сервис облачный и командный, поэтому подключать его не нужно,
но модель переносится в `~/.stepan_verify` почти один в один:

| Board | stepan_verify |
|---|---|
| task = thread + state labels | `tasks/<id>.md`: фронтматтер `status`, `labels`, `depends_on`, `sessions`; тело — описание + лог |
| claim / renew lease / complete | `claimed_by: claude:<session>` + `lease_until`; истёкшая аренда → задача снова свободна |
| ready-очередь, зависимости | задача «готова», когда все `depends_on` в `done` — её берёт следующий свободный агент |
| watchers (событие будит агента) | `automations/*.md`: «новая задача с label X», «PR получил ревью», «CI упал» → запуск сессии |
| schedule «проверь через 5 мин» | агент пишет в задачу `follow_up_at`, stepan_verify сам возобновит сессию |
| тред = журнал | append-only `state/events.jsonl` (кто, что, когда) → таймлайн на дашборде |
| grants | режим доступа на проект/автоматизацию (read-only / правки / полный) |

Главный риск, который Board прямо называет: текст задачи — это инструкция агенту, поэтому задачи,
пришедшие извне (GitHub/Slack), должны запускаться в режиме «спрашивать».
