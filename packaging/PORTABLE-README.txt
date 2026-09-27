FastPlay Portable for Windows x64
================================

Run fastplay.exe directly, pass a media path on the command line, or drag a
supported media file onto the FastPlay window.

This package does not install FastPlay. It does not create shortcuts or file
associations and does not require administrator access.

"Portable" describes the no-installer application bundle. FastPlay keeps its
settings, recent-file history, resume positions, session logs, and crash logs
under %APPDATA%\FastPlay so that the MSI and portable builds share the same
user state. Removing this extracted folder does not remove that user data.

FastPlay is a Windows x64 application. Keep fastplay.exe, ffmpeg.exe,
ffprobe.exe, and the bundled DLLs together in the same directory. The two
FFmpeg tools power marked In/Out clip export.

License information is in the licenses directory and
THIRD-PARTY-NOTICES.txt.
