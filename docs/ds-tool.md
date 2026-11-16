# ds-tool: projects for scripts and AI agents

ds-tool changes DicingSplicing projects from the command line. It puts pieces of recordings on the
timeline one after another, moves them, trims them, cuts them and takes them out, and tells what a
project has.
Every change goes through the editor's own code, keeps its rules, and is saved the way the editor
saves. A script or an AI agent needs to know the commands on this page, not the editor or its files.

## Where it is

In the editor's package, `ds-tool.exe` and this guide, `ds-tool.md`, are next to `dicing_splicing.exe`.
From the source, `cargo build --release` makes `target\release\ds-tool.exe`. `ds-tool help` says where
the guide is. ds-tool reads the recordings with ffprobe, which it looks for as the editor does: the path
in the editor's Settings, then the editor's own folder, then the PATH.

## Commands

```
ds-tool new <folder> [--name NAME] [--frame FRAME]
ds-tool build <folder> <list.json>
ds-tool append <project> <file> [--from TIME] [--to TIME] [--name NAME] [--fit]
ds-tool insert <project> <file> --before N [--from TIME] [--to TIME] [--name NAME] [--fit]
ds-tool delete <project> N
ds-tool move <project> N --to M
ds-tool trim <project> N [--from TIME] [--to TIME]
ds-tool blade <project> N --at TIME
ds-tool show <project>
ds-tool check <project>
ds-tool undo <project>
```

- `<project>` is a project's folder, or its `project.json`.
- `N` and `M` are clip numbers, as `show` lists them, 1 for the first.
- `TIME` is seconds (`75.5`), minutes and seconds (`1:15.5`), or hours too (`1:02:03`).
- `--json` after any command makes the answer JSON.
- `ds-tool help <command>` tells more about one.

What they do:

- **new** makes a project with nothing in it yet, in a folder it makes if it is not there. `--frame` is
  the frame the video is made for: `Landscape` (16:9, the default), `Vertical` (9:16), `Square`,
  `Tablet` (16:10), `Photo` (3:2), `Classic` (4:3), `Post` (4:5), `Cinema` (21:9) or
  `TelegramCircle`. The export settings are the editor's preset for that frame, or its YouTube preset.
- **build** makes a project from a list at once, as `new` and an `append` for each piece. When one
  piece can't go in, nothing is made.
- **append** puts a piece at the end, right after the last clip. Without `--from` it starts where the
  recording starts, without `--to` it ends where the recording ends. `--name` is what the clip is
  called on the timeline; without it, the file's name. The recording's sound comes with it. `--fit`
  shows all of the picture: a recording of other proportions than the frame, such as a 16:10 screen
  in the 16:9 frame, gets black bars instead of losing its edges, as Fit does in the editor.
- **insert** puts a piece before clip `N`; clip `N` and the clips after it move on by its length.
- **delete** takes clip `N` off; the clips after it move back by its length, so no gap is left.
- **move** makes clip `N` the clip `M`; the clips stay one after another.
- **trim** makes clip `N` play another part of its recording: `--from` and `--to` are times in the
  recording, and the one not given stays. The clips after it move by as much as it got longer or
  shorter.
- **blade** cuts clip `N` in two where its recording is at `--at`, as the Blade does in the editor:
  clip `N` keeps what is before the cut, and a new clip `N+1` right after it plays the rest, with the
  same name, framing and sound. Nothing moves.
- **show** tells the frame, the clips in order (where each starts, how long it is, what part of which
  file it plays), the other tracks, the export range, and what may not be meant, such as a gap.
- **check** also looks at the files: whether every recording is there and can be read, and whether
  the clips play only what the recordings have.
- **undo** takes back the last change ds-tool made, if nothing has changed the project since.

## A tutorial, step by step

```
ds-tool new D:/tutorials/lesson-03 --name "Lesson 3. Installing"
ds-tool append D:/tutorials/lesson-03 D:/rec/01_intro.mp4 --name Intro
ds-tool append D:/tutorials/lesson-03 D:/rec/02_install.mp4 --from 0:12 --to 4:05 --name Installing
ds-tool append D:/tutorials/lesson-03 D:/rec/03_result.mp4 --from 3.5 --name "The result"
ds-tool check D:/tutorials/lesson-03
```

Each change answers with what it did and the project as it is now:

```
Put clip 2, Installing: 02_install.mp4 from 00:12.00 to 04:05.00, 03:53.00 long.

Lesson 3. Installing: frame Landscape (16:9), Video 1 has 2 clips, 04:05.50 long.
    #  start     length    name        plays
    1  00:00.00  00:12.50  Intro       01_intro.mp4 00:00.00 to 00:12.50
    2  00:12.50  03:53.00  Installing  02_install.mp4 00:12.00 to 04:05.00
Export range: 00:00.00 to 04:05.50.
```

To take a part out of the middle of a recording, cut at both of its ends and delete the clip between
them. Here the installation is 0:12 to 4:05 of its recording, without 1:00 to 1:30:

```
ds-tool blade D:/tutorials/lesson-03 2 --at 1:00
ds-tool blade D:/tutorials/lesson-03 3 --at 1:30
ds-tool delete D:/tutorials/lesson-03 3
```

The same project at once, from a list:

```json
{
  "name": "Lesson 3. Installing",
  "frame": "Landscape",
  "pieces": [
    { "file": "01_intro.mp4", "name": "Intro" },
    { "file": "02_install.mp4", "from": "0:12", "to": "4:05", "name": "Installing", "fit": true },
    { "file": "03_result.mp4", "from": 3.5, "name": "The result" }
  ]
}
```

```
ds-tool build D:/tutorials/lesson-03 D:/rec/lesson-03.json
```

A file's path in the list is full, or goes from the list's folder; `from` and `to` are times as on
the command line, or numbers of seconds; `"fit": true` is `--fit`.

## What it answers

- The text: what was done, then the project. With `--json`, the same as an object: `done`, and
  `project` with `name`, `frame`, `ratio`, `clips` (each with `number`, `name`, `start`, `length`,
  `file`, `from` and `to`, in seconds, `speed`, `holds`, `still`, `image`, and `bars`, true for a
  picture shown whole with black bars), `length`, `export`, `other_tracks` and `warnings`. `check`
  adds `problems`. `holds` are the frames the clip holds on the way, each with `at`, its moment in
  the file, and `seconds`, how long it is held. `still` is true for a clip that shows one picture all
  along, and `image` for one of an image; `from` and `to` of such a clip are both the moment of its
  picture in the file.
- The exit code is 0 when it did what was asked. When it can't, it is 1, the reason is on the error
  output, and the project stays as it was. `check` answers 1 when it finds a problem.
- A mistake in how the command is written comes with how it is written.

## Rules

- ds-tool works on Video 1, the top video track. A project made by ds-tool has only that one; tracks
  added in the editor stay as they are, and `show` lists them.
- Videos only: a file without a picture, such as music, is refused, and so is an image. Images go
  onto the timeline in the editor; `show` lists such a clip as "an image", and `trim` and `blade`
  leave it to the editor.
- A frame held in a clip, made in the editor with Freeze frame, stays with the clip, which is that
  much longer: `show` tells it after the part the clip plays, "holding its frame at 00:12.40 for
  00:02.00". `blade` cuts it with the clip, and `trim` keeps it while its moment stays in the part
  played.
- A clip chained to another in the editor, as a picture to its detached sound, is left to the editor:
  delete, move, trim and blade refuse it rather than part them.
- While the editor has the project open, ds-tool changes nothing: the editor saves what it shows by
  itself, and would write over the change. Close the project in the editor first (Projects, at the
  top left). `show` and `check` work anyway. The editor holds `editor.lock` in the project's folder
  while the project is open.
- `undo` takes back only the last change, and only if nothing has changed the project since, the
  editor included.
- A recording of other proportions than the frame has its edges cut, as any new clip in the editor,
  unless it is put in with `--fit`. `show` marks a clip shown whole: "with black bars". In the editor,
  Fit does the same to any clip: it is in the menu that opens when the pointer rests on the frame's
  top mark.
- Recordings are linked, not copied: keep them where they are.
- A clip can be made faster or slower in the editor (the Speed effect). `show` says so: "at ×2"; it
  is half as long then. `--from`, `--to` and `--at` are still times in its recording.

## Asking an agent

Something like this does it, with the paths changed:

> Use ds-tool, DicingSplicing's command-line program: docs/ds-tool.md tells how, and `ds-tool help`
> lists its commands. The recordings of lesson 3 are in `D:/rec/lesson3`, and their order is in
> `D:/rec/lesson3/order.txt`: a line for each piece, with times where only part of a recording is
> needed. Make the project `D:/tutorials/lesson-03` in the Landscape frame, each piece right after the
> one before it, with `--fit` for screen recordings. Do not change the recordings. Run `ds-tool check`
> at the end, then tell me the pieces with where each one starts, the total length, and anything in
> the order you were not sure about.

To try it blind, tell the agent only where `ds-tool.exe` is and what to make: `ds-tool` with no
words, or `ds-tool help`, lists the commands and says where this guide is.

For what ds-tool does not do, a script can write the project file itself:
[project-file.md](project-file.md) describes it.
