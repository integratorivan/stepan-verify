<p align="center">
  <img src="crates/stepan_verify_app/assets/icon/icon-512.png" width="160" alt="Stepan">
</p>

<h1 align="center">Stepan</h1>

<p align="center">
  Нативное macOS-приложение (Rust + <a href="https://gpui-kit.com/">gpui-kit</a>) — пульт для агентов Claude Code и Codex.
</p>

<p align="center">
  <a href="https://github.com/integratorivan/stepan-verify/releases/latest/download/stepan-macos-arm64.dmg"><b>Скачать для Apple Silicon</b></a>
  ·
  <a href="https://github.com/integratorivan/stepan-verify/releases">все релизы</a>
</p>

- **Дашборд**: живые сессии карточками («Пульт») — шаги, текущий шаг, «нужен ответ / нужно подтверждение» с решением прямо на карточке.
- **Сессии**: лента как в t3code — сообщения, инструменты, диффы, картинки; запуск и продолжение Claude Code и Codex. Агенты переживают закрытие и обновление Stepan: доделывают ход сами, а открытое снова окно подключается к ним вместе с неотвеченными запросами. Сообщения, написанные во время хода, ждут в очереди; «Отправить сейчас» прерывает шаг и отправляет сразу. Если процесс агента всё же умер посреди хода, сессия помечается «прервана» с кнопкой «Продолжить».
- **Изменения**: «Архитектура» — модули и импорты «было / стало / разница», посчитанные из файлов; «Контракты» — типы, сигнатуры, зависимости и lock-файлы; «До / После» — скриншоты, GIF и видео. Итерации и батч-обновления длинной сессии с решениями агента; смысл (к чему идём, зачем каждый модуль) пишет агент того же семейства, что и сессия.
- **Задачи**: доска в духе Vercel Board — аренда задач агентами, автопилот, `stepan_verify task …` CLI.
- **Документы** («Полка»): разборы, решения, фактура проектов и вопросы к тебе. Агенты пишут их сами по правилам, которые Stepan даёт каждой сессии, а после сессии отдельный агент записывает итоги.
- **Скиллы** («Связи»: кто видит скилл и как Claude/Codex его используют).
- **Чем пользуюсь**: какие фичи за 30 дней трогали ты и агенты, а какие никто; `stepan_verify usage [--idle]` в терминале. Считается локально, ничего не уходит с компьютера.
- Общая папка `~/.stepan_verify`: skills, tasks, docs, automations, state. Данные из старой `~/.iv3` переносятся сами при первом запуске.

## Установка

1. Скачай `.dmg` по ссылке выше и перетащи **Stepan** в «Программы».
2. Первый запуск: правый клик по Stepan → **Открыть** → **Открыть**. Сборка подписана ad-hoc, без нотаризации Apple, поэтому macOS спросит один раз. Если пишет «повреждено»: `xattr -dr com.apple.quarantine /Applications/Stepan.app`.
3. Нужны установленные `claude` и/или `codex` — Stepan берёт `PATH` из твоего login-shell, так что Homebrew, npm и mise видны и при запуске из Dock.

**Обновления** приходят сами: Stepan проверяет GitHub Releases при запуске, каждые 15 минут и когда окно снова становится активным. О новой версии сообщает уведомление на любой странице (и в Центре уведомлений, если Stepan в фоне), а в сайдбаре остаётся «Обновить»: Stepan скачает её, заменит себя и перезапустится. Выключить — Настройки → Обновления.

## Сборка из исходников

```sh
cargo build -p stepan_verify_app        # Rust 1.100 (rust-toolchain.toml)
./target/debug/stepan_verify            # окно
./target/debug/stepan_verify task list  # CLI доски
scripts/bundle.sh             # dist/Stepan.app, .zip и .dmg для этого Mac
```

## Релиз

1. Подними `version` в `Cargo.toml` (`[workspace.package]`) и закоммить.
2. `git tag v0.2.0 && git push origin v0.2.0`.
3. GitHub Actions (`.github/workflows/release.yml`) соберёт Stepan для Apple Silicon (Intel не собираем) и опубликует релиз с `stepan-macos-arm64.zip` (их качает апдейтер) и `.dmg` (для людей). Установленные копии увидят его сами.

Подробности — `docs/ROADMAP.md`.

Шрифт интерфейса по умолчанию — [Instrument Sans](https://github.com/Instrument/instrument-sans) (SIL Open Font License, `crates/stepan_verify_app/assets/fonts/OFL.txt`); шрифты текста и кода меняются в настройках.
