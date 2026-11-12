#!/usr/bin/env python3
"""Static checks for scripts/windows/sar-launcher.bat.

CI has no Windows runner. This checks the file shape the .bat depends on:
ASCII, CRLF, the URL concatenation cmd actually parses, one error prefix,
the lock TTL test, and a lock delete on every exit that owns the lock.
"""

import pathlib
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
PATH = ROOT / "scripts" / "windows" / "sar-launcher.bat"


def expand_percents(text, args, env):
    """cmd.exe phase 1 for the URL line: %% , %~n, %n, %VAR%."""
    out = []
    i = 0
    n = len(text)
    while i < n:
        if text[i] != "%":
            out.append(text[i])
            i += 1
            continue
        if i + 1 < n and text[i + 1] == "%":
            out.append("%")
            i += 2
            continue
        if i + 1 < n and text[i + 1] == "~":
            j = i + 2
            while j < n and text[j].lower() in "fdpnxsatz":
                j += 1
            if j < n and text[j].isdigit():
                val = args.get(int(text[j]), "")
                if len(val) >= 2 and val[0] == '"' and val[-1] == '"':
                    val = val[1:-1]
                out.append(val)
                i = j + 1
                continue
            raise SystemExit("bad %~ argument in: " + text)
        if i + 1 < n and text[i + 1].isdigit():
            out.append(args.get(int(text[i + 1]), ""))
            i += 2
            continue
        j = text.find("%", i + 1)
        if j < 0:
            raise SystemExit("unclosed % in: " + text)
        out.append(env.get(text[i + 1 : j], ""))
        i = j + 1
    return "".join(out)


def main():
    raw = PATH.read_bytes()
    if any(b > 127 for b in raw):
        raise SystemExit("sar-launcher.bat is not ASCII")
    if b"\n" in raw.replace(b"\r\n", b""):
        raise SystemExit("sar-launcher.bat must use CRLF only")
    if b"\r\n" not in raw:
        raise SystemExit("sar-launcher.bat has no CRLF line endings")
    text = raw.decode("ascii")
    if "[ERRO]" in text:
        raise SystemExit("use [ERROR], not [ERRO]")

    url_lines = [ln for ln in text.splitlines() if ln.startswith('set "_URL=')]
    if url_lines != ['set "_URL=%BASE_URL%/%~1%URL_SUFFIX%"']:
        raise SystemExit("URL line is not BASE_URL/file + URL_SUFFIX: " + repr(url_lines))
    expanded = expand_percents(
        url_lines[0],
        {1: '"smoke_test.py"'},
        {
            "BASE_URL": "https://www.domain.example/files",
            "URL_SUFFIX": "/0.0/0",
        },
    )
    want = 'set "_URL=https://www.domain.example/files/smoke_test.py/0.0/0"'
    if expanded != want:
        raise SystemExit("URL expanded to %r, want %r" % (expanded, want))

    lock = [
        ln
        for ln in text.splitlines()
        if "LOCK_TTL" in ln and "LastWriteTime" in ln and "exit 1" in ln and "exit 0" in ln
    ]
    if len(lock) != 1 or "AddMinutes(-[int]$env:LOCK_TTL)" not in lock[0]:
        raise SystemExit("stale lock is not the LastWriteTime vs LOCK_TTL test")
    # Younger than the TTL is held (exit 1). Equal or older falls through and is replaced.
    if not lock[0].strip().endswith("exit 0\""):
        raise SystemExit("lock command must end by taking the lock (exit 0)")

    if 'if errorlevel 1 set "HELPER_FAIL=1"' not in text:
        raise SystemExit("helper loop must use runtime 'if errorlevel 1', not %errorlevel%")
    loop = text.split("for %%H in (%HELPERS%) do (", 1)[1].split(")", 1)[0]
    if "%errorlevel%" in loop:
        raise SystemExit("helper loop reads %errorlevel%, which is stale inside the block")

    held = text.split("echo [LOCK]", 1)[1].split("exit /b 2", 1)[0]
    if "del " in held:
        raise SystemExit("a held lock must not be deleted")
    for needle in (
        'echo [ERROR] Python not in PATH. & del "%LOCK_FILE%" 2>nul & exit /b 1',
        'echo [ERROR] Script not found. Aborting...\r\n    del "%LOCK_FILE%" 2>nul\r\n    exit /b 1',
        'del "%LOCK_FILE%" 2>nul\r\necho Finishing with code %RC%.\r\nendlocal & exit /b %RC%',
        "exit /b %errorlevel%",
    ):
        if needle not in text:
            raise SystemExit("missing lock-release path: " + needle.splitlines()[0])
    # The subroutine must return. A bare exit would skip the caller's lock delete.
    if "\nexit %errorlevel%" in text.replace("\r\n", "\n"):
        raise SystemExit(":fetch must exit /b so the caller can delete the lock")

    print("sar-launcher.bat ok")


if __name__ == "__main__":
    try:
        main()
    except SystemExit as exc:
        if exc.code not in (0, None):
            print(exc, file=sys.stderr)
        raise
