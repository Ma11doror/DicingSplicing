DicingSplicing
==============

A simple video editor for short videos: Telegram circles, Reels, Shorts, TikTok.
https://github.com/Ma11doror/DicingSplicing

Run dicing_splicing.exe. Nothing has to be installed: everything it needs is in this folder.

First start
-----------
Windows may say it protected your PC: the program is not signed. Choose "More info", then
"Run anyway".

The editor asks for a folder for your projects. It keeps the projects there, the frames it
prepares for playback and the videos downloaded by link, so it needs some free space.

Press F1 in the editor for the list of keys.

In this folder
--------------
dicing_splicing.exe            the editor
dicing_splicing-link.exe       plugin: videos by link (YouTube, song.link and others)
dicing_splicing-telegram.exe   plugin: sends a finished video to Telegram
dicing_splicing.pdb            names for the error reports
ffmpeg.exe, ffprobe.exe        FFmpeg: reads and writes video (ffmpeg-LICENSE.txt, ffmpeg-README.txt)
yt-dlp.exe                     downloads by link, when it is here
LICENSE.txt, NOTICE.txt        the editor's license: Apache License 2.0
THIRD-PARTY-LICENSES.txt       the licenses of the libraries built into the programs

If videos by link stop downloading, update yt-dlp: run "yt-dlp.exe -U" in this folder. A yt-dlp
installed separately (winget install yt-dlp.yt-dlp) is used before the one here.

Telegram
--------
Sending to Telegram needs your own bot and channels, in a credentials.json next to
dicing_splicing-telegram.exe; the README on GitHub shows how to write it. Without it everything
else works, export to a file too.

Problems
--------
If the editor closes on an error, it writes a report to %APPDATA%\DicingSplicing\logs. Please
attach it to an issue on GitHub, with the steps that led to it.
