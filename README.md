# Volna

Простой музыкальный плеер для Linux с отображением трека в виде волны.
GTK4 + libadwaita + GStreamer, написан на Rust.

Сайт: https://jic8.github.io/volna/

![Volna](data/screenshots/player.png)

- Плейлисты: несколько файлов, папки, сохранение и загрузка M3U
- Перемотка кликом по волне, ±10 секунд
- Скорость 0.5×–2× без изменения тона
- Очередь восстанавливается при следующем запуске
- Управление из панели задач, виджетов и медиаклавишами (MPRIS)

## Сборка

```sh
cargo run --release
```

Нужны GTK 4.14+, libadwaita 1.6+ и GStreamer 1.24+ с плагинами base/good.

### Flatpak

```sh
flatpak run org.flatpak.Builder --user --install-deps-from=flathub \
    --force-clean --install build-dir build-aux/io.github.jic8.Volna.json
flatpak run io.github.jic8.Volna
```

После изменения зависимостей пересоздайте `build-aux/cargo-sources.json`
с помощью [flatpak-cargo-generator](https://github.com/flatpak/flatpak-builder-tools/tree/master/cargo).

## Переводы

Исходные строки — на английском, переводы лежат в `po/`. Обновить шаблон и русский перевод:

```sh
xgettext --language=Rust --from-code=UTF-8 --keyword=gettext --keyword=ngettext:1,2 \
    --files-from=po/POTFILES -o po/volna.pot
msgmerge --update po/ru.po po/volna.pot
```

Новый язык: `msginit -i po/volna.pot -l <код>`, затем добавить код в `po/LINGUAS`.

## Сайт

Страница проекта лежит в `docs/` и публикуется через GitHub Pages
(Settings → Pages → Deploy from a branch → `main` / `docs`).

## Лицензия

GPL-3.0-or-later
