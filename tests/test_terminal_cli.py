"""Unit tests for the Python host CLI (tools/linkr_ble_terminal.py).

Run with: python3 -m unittest discover -s tests -v

The module is loaded straight from disk. It must import without bleak
installed: --help/--version, the argument validators and the pure protocol
helpers never touch the radio.
"""

import asyncio
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
import types
import unittest
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[1]
TERMINAL_PATH = ROOT / "tools/linkr_ble_terminal.py"


def load_terminal():
    spec = importlib.util.spec_from_file_location("linkr_terminal_cli", TERMINAL_PATH)
    module = importlib.util.module_from_spec(spec)
    with patch.dict(sys.modules, {spec.name: module}):
        spec.loader.exec_module(module)
    return module


terminal = load_terminal()


def cfg(**overrides):
    values = dict(escape=b"\x1d", enter="raw", local_echo=False, line_mode=False,
                  debug_io=False, write_size=20, write_response=False,
                  write_delay=0.0)
    values.update(overrides)
    return terminal.TerminalConfig(**values)


def mgmt_frame(message_type, request_id, payload, flags=0):
    return terminal.MGMT_HEADER.pack(
        b"LK", terminal.MGMT_API_MAJOR, message_type, request_id, len(payload), flags
    ) + payload


def uart_frame(sequence, payload):
    return terminal.RELIABLE_UART_HEADER.pack(
        b"LR", 1, 0, sequence, len(payload), 0
    ) + payload


class FakeDevice:
    def __init__(self, name, address, rssi=None):
        self.name = name
        self.address = address
        self.rssi = rssi


class FakeWriteClient:
    def __init__(self):
        self.writes = []

    async def write_gatt_char(self, uuid, chunk, response=True):
        self.writes.append((uuid, bytes(chunk)))


class ParserTests(unittest.TestCase):
    """The CLI must be usable before bleak is installed."""

    def run_cli(self, *arguments, env=None):
        return subprocess.run(
            [sys.executable, str(TERMINAL_PATH), *arguments],
            capture_output=True, text=True, timeout=30,
            stdin=subprocess.DEVNULL, env=env,
        )

    def test_help_works_without_bleak(self):
        result = self.run_cli("--help")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("usage:", result.stdout)
        self.assertIn("--wifi-key-file", result.stdout)
        self.assertNotIn("Traceback", result.stderr)

    def test_version_works_without_bleak(self):
        result = self.run_cli("--version")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(terminal.__version__, result.stdout)

    def test_bad_uart_spec_is_rejected_before_touching_the_radio(self):
        result = self.run_cli("--uart", "abc,8,n,1,n")
        self.assertEqual(result.returncode, 2)
        self.assertIn("baud", result.stderr)
        self.assertNotIn("bleak", result.stderr.lower())

    def test_missing_bleak_is_a_clean_error(self):
        module = load_terminal()
        with patch.dict(sys.modules, {"bleak": None}):
            with self.assertRaisesRegex(RuntimeError, "pip install bleak"):
                module.import_bleak()

    def test_missing_wifi_password_is_reported_before_the_radio_is_touched(self):
        env = {key: value for key, value in os.environ.items()
               if key != terminal.WIFI_PASSWORD_ENV}
        result = self.run_cli("--wifi", "MySSID", "--no-terminal",
                              "--address", "AA:BB", env=env)
        self.assertEqual(result.returncode, 2)
        self.assertIn("no WiFi password available", result.stderr)
        self.assertNotIn("bleak", result.stderr.lower())

    def test_uart_spec_accepts_spellings_the_firmware_accepts(self):
        parser = terminal.build_parser()
        args = parser.parse_args(["--uart", "115200, 8, none, 1, hw"])
        self.assertEqual(args.uart, "115200,8,n,1,rtscts")


class CompletionTests(unittest.TestCase):
    """--print-completion must stay in step with the parser it describes."""

    def script(self, shell):
        return terminal.completion_script(shell, terminal.build_parser())

    def parser_options(self):
        return {
            option
            for action in terminal.build_parser()._actions
            for option in action.option_strings
        }

    def bash_complete(self, *words):
        """Drive the generated function in a real bash, as completion would."""
        with tempfile.TemporaryDirectory(prefix="linkr-completion-") as directory:
            path = Path(directory) / "completion.bash"
            path.write_text(self.script("bash"))
            program = "\n".join([
                f"source {shlex.quote(str(path))}",
                "COMP_WORDS=(" + " ".join(shlex.quote(word) for word in words) + ")",
                "COMP_CWORD=$(( ${#COMP_WORDS[@]} - 1 ))",
                "COMPREPLY=()",
                "_linkr_ble_terminal",
                'printf "%s\\n" "${COMPREPLY[@]}"',
            ])
            result = subprocess.run(["bash", "-c", program],
                                    capture_output=True, text=True, timeout=30)
        self.assertEqual(result.returncode, 0, result.stderr)
        return result.stdout.split()

    @unittest.skipUnless(shutil.which("bash"), "bash is required")
    def test_bash_script_is_syntactically_valid(self):
        with tempfile.TemporaryDirectory(prefix="linkr-completion-") as directory:
            path = Path(directory) / "completion.bash"
            path.write_text(self.script("bash"))
            result = subprocess.run(["bash", "-n", str(path)],
                                    capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)

    @unittest.skipUnless(shutil.which("zsh"), "zsh is required")
    def test_zsh_script_is_syntactically_valid(self):
        with tempfile.TemporaryDirectory(prefix="linkr-completion-") as directory:
            path = Path(directory) / "_linkr_ble_terminal"
            path.write_text(self.script("zsh"))
            result = subprocess.run(["zsh", "-n", str(path)],
                                    capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)

    @unittest.skipUnless(shutil.which("fish"), "fish is required")
    def test_fish_script_is_syntactically_valid(self):
        with tempfile.TemporaryDirectory(prefix="linkr-completion-") as directory:
            path = Path(directory) / "linkr_ble_terminal.py.fish"
            path.write_text(self.script("fish"))
            result = subprocess.run(["fish", "--no-execute", str(path)],
                                    capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_fish_registers_every_option_for_every_command_name(self):
        lines = [line for line in self.script("fish").splitlines()
                 if line.startswith("complete ")]
        commands = terminal.COMPLETION_COMMANDS
        self.assertEqual(len(lines), len(self.parser_options()) * len(commands))

        for option in sorted(self.parser_options()):
            # fish spells the option without its dashes: --enter is -l enter.
            if option.startswith("--"):
                expected = f"-l {option[2:]}"
            else:
                expected = f"-s {option[1:]}"
            for command in commands:
                with self.subTest(option=option, command=command):
                    self.assertTrue(
                        any(line.startswith(f"complete -c {command} ") and
                            expected in line for line in lines),
                        f"{command} does not register {option}")

    def test_fish_marks_choices_files_and_plain_values(self):
        script = self.script("fish")
        # -x is fish's "requires a value, complete no files".
        self.assertIn("-l enter -x -a 'raw cr lf crlf'", script)
        self.assertIn("-l print-completion -x -a 'bash fish zsh'", script)
        self.assertIn("-l log-file -r -F", script)
        self.assertIn("-l wifi-key-file -r -F", script)
        self.assertIn("-l name -r -f", script)

    def test_fish_script_keeps_descriptions_quoted(self):
        for line in self.script("fish").splitlines():
            if not line.startswith("complete "):
                continue
            with self.subTest(line=line):
                # Exactly the opening and closing quote, once the choice list
                # and any escaped apostrophe (argparse's own --version text has
                # one) are set aside. A stray quote would end the string early.
                unescaped = re.sub(r"-a '[^']*'", "", line.replace("\\'", ""))
                self.assertEqual(unescaped.count("'"), 2)

    @unittest.skipUnless(shutil.which("fish"), "fish is required")
    def test_fish_completion_answers_the_way_a_shell_needs(self):
        """Drive fish's own completion engine, hermetically.

        fish only autoloads a completion file for a command it can find, so the
        probe puts a stub on PATH; XDG_CONFIG_HOME keeps it out of the real
        ~/.config/fish.
        """
        with tempfile.TemporaryDirectory(prefix="linkr-fish-") as directory:
            config = Path(directory) / "config"
            commands = config / "fish" / "completions"
            commands.mkdir(parents=True)
            script = self.script("fish")
            for name in terminal.COMPLETION_COMMANDS:
                (commands / f"{name}.fish").write_text(script)

            stub_dir = Path(directory) / "bin"
            stub_dir.mkdir()
            for name in terminal.COMPLETION_COMMANDS:
                stub = stub_dir / name
                stub.write_text("#!/bin/sh\nexit 0\n")
                stub.chmod(0o755)

            env = dict(os.environ,
                       XDG_CONFIG_HOME=str(config),
                       PATH=f"{stub_dir}{os.pathsep}{os.environ.get('PATH', '')}")

            def complete(line):
                result = subprocess.run(["fish", "-c", f'complete -C"{line}"'],
                                        capture_output=True, text=True,
                                        env=env, timeout=30)
                self.assertEqual(result.returncode, 0, result.stderr)
                return sorted(entry.split("\t")[0]
                              for entry in result.stdout.splitlines())

            for command in terminal.COMPLETION_COMMANDS:
                with self.subTest(command=command):
                    self.assertEqual(
                        complete(f"{command} --enter "),
                        ["cr", "crlf", "lf", "raw"])
                    self.assertEqual(
                        complete(f"{command} --print-completion "),
                        ["bash", "fish", "zsh"])
                    # A free-form value is neither completed with files nor
                    # with anything else.
                    self.assertEqual(complete(f"{command} --name "), [])

    def test_every_parser_option_reaches_both_scripts(self):
        options = self.parser_options()
        self.assertTrue(options)
        for shell in terminal.COMPLETION_SHELLS:
            script = self.script(shell)
            for option in sorted(options):
                with self.subTest(shell=shell, option=option):
                    if shell == "fish":
                        # fish spells an option without its dashes.
                        expected = (f"-l {option[2:]}" if option.startswith("--")
                                    else f"-s {option[1:]}")
                    else:
                        expected = option
                    self.assertIn(expected, script)

    def test_option_choices_are_completed(self):
        for shell in terminal.COMPLETION_SHELLS:
            with self.subTest(shell=shell):
                script = self.script(shell)
                self.assertIn("raw cr lf crlf", script)
        # zsh needs the action list, bash the compgen word list.
        self.assertIn("(raw cr lf crlf)", self.script("zsh"))
        self.assertIn('--enter) COMPREPLY=( $(compgen -W "raw cr lf crlf"',
                      self.script("bash"))

    def test_path_options_complete_files(self):
        script = self.script("bash")
        self.assertIn("--wifi-key-file|--log-file) COMPREPLY=( $(compgen -f",
                      script)
        self.assertIn("--log-file=[append raw BLE RX bytes to a file]:file:_files",
                      self.script("zsh"))

    @unittest.skipUnless(shutil.which("bash"), "bash is required")
    def test_bash_completion_answers_the_way_a_shell_needs(self):
        self.assertEqual(self.bash_complete("linkr_ble_terminal.py", "--e"),
                         ["--enter", "--escape"])
        self.assertEqual(self.bash_complete("linkr_ble_terminal.py", "--enter", "c"),
                         ["cr", "crlf"])
        self.assertEqual(
            self.bash_complete("linkr_ble_terminal.py", "--print-completion", "z"),
            ["zsh"])
        # A prefix that several options share returns all of them.
        self.assertEqual(self.bash_complete("linkr_ble_terminal.py", "--json", "--q"),
                         ["--query-info", "--query-uart", "--query-wifi",
                          "--query-webdav", "--quiet"])
        self.assertEqual(self.bash_complete("linkr_ble_terminal.py", "--quiet", "--j"),
                         ["--json"])
        # --flag=value is accepted by the parser, so the value completes too.
        self.assertEqual(self.bash_complete("linkr_ble_terminal.py", "--enter=c"),
                         ["--enter=cr", "--enter=crlf"])
        self.assertEqual(
            self.bash_complete("linkr_ble_terminal.py", "--print-completion=f"),
            ["--print-completion=fish"])

    def test_unsupported_shell_is_a_usage_error(self):
        with self.assertRaises(ValueError):
            self.script("ksh")

    def test_completion_prints_without_bleak_and_leaves_the_radio_alone(self):
        # This interpreter has no bleak installed: printing a script must not
        # need it, and must not be confused with a device operation.
        result = subprocess.run(
            [sys.executable, str(TERMINAL_PATH), "--print-completion", "bash"],
            capture_output=True, text=True, timeout=30)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("complete -F _linkr_ble_terminal", result.stdout)

    def test_a_bad_shell_exits_two_from_the_command_line(self):
        result = subprocess.run(
            [sys.executable, str(TERMINAL_PATH), "--print-completion", "ksh"],
            capture_output=True, text=True, timeout=30)
        self.assertEqual(result.returncode, 2)
        # argparse changed the wording of this message across versions.
        self.assertIn("invalid choice", result.stderr)
        for shell in terminal.COMPLETION_SHELLS:
            self.assertIn(shell, result.stderr)


class ScanRuleTests(unittest.TestCase):
    """--scan lists and exits unless the invocation asked for more."""

    def parse(self, *argv):
        return terminal.build_parser().parse_args(list(argv))

    def test_scan_alone_is_the_whole_job(self):
        args = self.parse("--scan")
        self.assertFalse(terminal.has_control_action(args))
        self.assertIsNone(args.address)

    def test_scan_with_a_control_action_continues(self):
        for extra in (["--query-info"], ["--pair"], ["--loopback-test"],
                      ["--wifi", "ssid,pass"], ["--uart", "115200,8,n,1,n"],
                      ["--wifi-scan"], ["--webdav", "http://host/dav/"]):
            with self.subTest(extra=extra):
                self.assertTrue(
                    terminal.has_control_action(self.parse("--scan", *extra)))

    def test_scan_with_an_explicit_address_continues(self):
        args = self.parse("--scan", "--address", "AA:BB:CC:DD:EE:FF")
        self.assertEqual(args.address, "AA:BB:CC:DD:EE:FF")

    def test_a_bare_terminal_invocation_has_no_control_action(self):
        args = self.parse()
        self.assertFalse(terminal.has_control_action(args))


class ValidatorTests(unittest.TestCase):
    def test_normalize_uart_spec_canonicalizes(self):
        self.assertEqual(
            terminal.normalize_uart_spec("9600,7,even,2,off"), "9600,7,e,2,n")

    def test_normalize_uart_spec_rejects_bad_fields(self):
        bad = [
            "115200,8,n,1",             # too few fields
            "299,8,n,1,n",              # baud below the firmware minimum
            "3000001,8,n,1,n",          # baud above the firmware maximum
            "115200,9,n,1,n",           # data bits
            "115200,8,n,3,n",           # stop bits
            "115200,8,mark,1,n",        # parity
            "115200,8,n,1,xonxoff",     # flow control
        ]
        for spec in bad:
            with self.subTest(spec=spec):
                with self.assertRaises(Exception):
                    terminal.normalize_uart_spec(spec)

    def test_parse_escape_forms(self):
        self.assertEqual(terminal.parse_escape("^]"), b"\x1d")
        self.assertEqual(terminal.parse_escape("0x1d"), b"\x1d")
        self.assertEqual(terminal.parse_escape("q"), b"q")
        for bad in ["^", "0x1ff", "ab", "0xzz"]:
            with self.subTest(value=bad):
                with self.assertRaises(Exception):
                    terminal.parse_escape(bad)

    def test_describe_escape_names_the_configured_byte(self):
        self.assertEqual(terminal.describe_escape(b"\x1d"), "Ctrl-]")
        self.assertEqual(terminal.describe_escape(b"\x03"), "Ctrl-C")
        self.assertEqual(terminal.describe_escape(b"q"), "'q'")

    def test_translate_enter_modes(self):
        self.assertEqual(terminal.translate_enter(b"a\r\nb\r", "raw"), b"a\r\nb\r")
        self.assertEqual(terminal.translate_enter(b"a\r\nb\r", "lf"), b"a\nb\n")
        self.assertEqual(terminal.translate_enter(b"a\n", "cr"), b"a\r")
        self.assertEqual(terminal.translate_enter(b"a\n", "crlf"), b"a\r\n")


class TerminalGeometryTests(unittest.TestCase):
    """The CLI mirrors web/terminal_geometry.js; these are the drift guards."""

    def test_geometry_is_clamped_like_the_web_client(self):
        self.assertEqual(terminal.terminal_geometry(0, 0)["key"], "2x2")
        self.assertEqual(terminal.terminal_geometry(5000, 5000)["key"], "1000x1000")
        self.assertEqual(terminal.terminal_geometry(None, None)["key"], "2x2")
        self.assertEqual(terminal.terminal_geometry(120, 30)["key"], "120x30")

    def test_geometry_command_is_byte_identical_to_the_web_client(self):
        self.assertEqual(
            terminal.terminal_geometry_command(80, 24),
            "stty rows 24 cols 80 >/dev/null 2>&1\r",
        )
        web = (ROOT / "web/terminal_geometry.js").read_text()
        self.assertIn(
            "stty rows ${geometry.rows} cols ${geometry.cols} >/dev/null 2>&1\\r",
            web,
        )

    def test_prompt_detection_matches_the_web_client_rules(self):
        for text in ["user@host:~$ ", "# ", "/srv/app$ ", "sh-5.1$ ", "[root@host]# "]:
            with self.subTest(text=text):
                self.assertTrue(terminal.looks_like_shell_prompt(text))
        for text in ["", "$ x", "running", "Loading kernel..."]:
            with self.subTest(text=text):
                self.assertFalse(terminal.looks_like_shell_prompt(text))

        web = (ROOT / "web/terminal_geometry.js").read_text()
        self.assertIn(r"/[$#] $/", web)
        self.assertIn(r"/@\S+(?::\S*)?$/", web)

    def test_geometry_sync_waits_for_an_idle_prompt_and_dedupes(self):
        sync = terminal.TerminalGeometrySync(80, 24)
        sync.observe("booting the target\r\n")
        self.assertIsNone(sync.take_pending_command())

        sync.observe("root@target:~$ ")
        command = sync.take_pending_command()
        self.assertEqual(command, "stty rows 24 cols 80 >/dev/null 2>&1\r")
        # In flight: a second prompt must not queue a duplicate.
        self.assertIsNone(sync.take_pending_command())
        sync.confirm_sent()
        sync.observe("root@target:~$ ")
        self.assertIsNone(sync.take_pending_command())

    def test_geometry_sync_resends_after_a_resize(self):
        sync = terminal.TerminalGeometrySync(80, 24)
        sync.observe("root@target:~$ ")
        sync.take_pending_command()
        sync.confirm_sent()

        sync.set_size(120, 40)
        self.assertIsNone(sync.take_pending_command())
        sync.observe("root@target:~$ ")
        self.assertEqual(
            sync.take_pending_command(),
            "stty rows 40 cols 120 >/dev/null 2>&1\r",
        )

    def test_geometry_sync_stops_at_a_prompt_that_is_not_idle(self):
        sync = terminal.TerminalGeometrySync(80, 24)
        sync.observe("root@target:~$ ")
        sync.mark_busy()
        self.assertIsNone(sync.take_pending_command())

    def test_geometry_sync_retries_after_a_failed_send(self):
        sync = terminal.TerminalGeometrySync(80, 24)
        sync.observe("root@target:~$ ")
        sync.take_pending_command()
        sync.abort_sent()
        sync.observe("root@target:~$ ")
        self.assertIsNotNone(sync.take_pending_command())


class WifiCredentialTests(unittest.TestCase):
    def test_inline_credentials_still_work(self):
        self.assertEqual(
            terminal.resolve_wifi_credentials("ssid,secret"), ("ssid", "secret"))

    def test_password_is_read_from_a_key_file(self):
        with contextlib.ExitStack() as stack:
            path = stack.enter_context(
                __import__("tempfile").TemporaryDirectory())
            key = Path(path) / "wifi.key"
            key.write_text("filesecret\nignored\n")
            self.assertEqual(
                terminal.resolve_wifi_credentials("ssid", str(key)),
                ("ssid", "filesecret"),
            )

    def test_password_can_come_from_the_environment(self):
        self.assertEqual(
            terminal.resolve_wifi_credentials("ssid", None,
                                              environ={"LINKR_WIFI_PASSWORD": "env"}),
            ("ssid", "env"),
        )

    def test_password_prompt_is_the_last_resort(self):
        self.assertEqual(
            terminal.resolve_wifi_credentials("ssid", None, environ={},
                                              prompt=lambda _: "typed"),
            ("ssid", "typed"),
        )

    def test_missing_password_is_an_error_not_an_empty_password(self):
        with self.assertRaises(ValueError):
            terminal.resolve_wifi_credentials("ssid", None, environ={}, prompt=None)

    def test_empty_ssid_and_empty_key_file_are_rejected(self):
        with self.assertRaises(ValueError):
            terminal.resolve_wifi_credentials(",secret")
        with contextlib.ExitStack() as stack:
            path = stack.enter_context(__import__("tempfile").TemporaryDirectory())
            key = Path(path) / "wifi.key"
            key.write_text("")
            with self.assertRaises(ValueError):
                terminal.resolve_wifi_credentials("ssid", str(key))


class DeviceMatchTests(unittest.TestCase):
    def test_exact_name_wins_over_prefix(self):
        devices = [FakeDevice("Linkr BLE UART 2", "AA:2"),
                   FakeDevice("Linkr BLE UART", "AA:1")]
        self.assertEqual(
            terminal.match_device(devices, "Linkr BLE UART"), "AA:1")

    def test_prefix_matches_the_default_name(self):
        devices = [FakeDevice("Other", "BB:1"),
                   FakeDevice("Linkr BLE UART 7F2C", "AA:7")]
        self.assertEqual(
            terminal.match_device(devices, "Linkr BLE UART*"), "AA:7")

    def test_no_match_returns_none(self):
        self.assertIsNone(terminal.match_device([FakeDevice("Other", "BB:1")], "x*"))

    def test_unnamed_devices_are_still_listed(self):
        devices = [FakeDevice(None, "AA:1", -70), FakeDevice("Named", "AA:2", None)]

        class FakeScanner:
            @staticmethod
            async def discover(timeout):
                return devices

        buffer = io.StringIO()
        with patch.object(terminal, "BleakClient", object), \
                patch.object(terminal, "BleakScanner", FakeScanner), \
                contextlib.redirect_stdout(buffer):
            asyncio.run(terminal.scan_devices(0.1))

        lines = buffer.getvalue().splitlines()
        self.assertEqual(len(lines), 2)
        self.assertTrue(any("(unknown)" in line for line in lines))
        self.assertTrue(any("-70 dBm" in line for line in lines))

    def test_find_device_reuses_a_completed_scan(self):
        class ExplodingScanner:
            @staticmethod
            async def discover(timeout):  # pragma: no cover - must not be called
                raise AssertionError("find_device scanned twice")

        devices = [FakeDevice("Linkr BLE UART 7F2C", "AA:7")]
        with patch.object(terminal, "BleakClient", object), \
                patch.object(terminal, "BleakScanner", ExplodingScanner):
            self.assertEqual(
                asyncio.run(terminal.find_device("Linkr BLE UART*", None, 0.1,
                                                 devices=devices)),
                "AA:7",
            )

    def test_explicit_address_skips_the_scan(self):
        self.assertEqual(
            asyncio.run(terminal.find_device("x", "AA:9", 0.1, devices=[])), "AA:9")


class ManagementChannelTests(unittest.IsolatedAsyncioTestCase):
    def quiet(self):
        """ManagementChannel narrates to stderr; keep test output readable."""
        return contextlib.redirect_stderr(io.StringIO())

    async def test_response_resolves_the_matching_future(self):
        channel = terminal.ManagementChannel(FakeWriteClient(), cfg(), 512)
        with self.quiet():
            task = asyncio.create_task(channel.send(b"@i?"))
            await asyncio.sleep(0)
            channel.on_indication(None, bytearray(mgmt_frame(2, 1, b"OK uptime=1\r\n")))
        self.assertEqual(await task, b"OK uptime=1\r\n")

    async def test_error_flag_raises(self):
        channel = terminal.ManagementChannel(FakeWriteClient(), cfg(), 512)
        with self.quiet():
            task = asyncio.create_task(channel.send(b"@u?"))
            await asyncio.sleep(0)
            channel.on_indication(None, bytearray(
                mgmt_frame(2, 1, b"ERR format\r\n", terminal.MGMT_FLAG_ERROR)))
        with self.assertRaises(RuntimeError):
            await task

    async def test_fragmented_response_is_reassembled(self):
        channel = terminal.ManagementChannel(FakeWriteClient(), cfg(), 512)
        with self.quiet():
            task = asyncio.create_task(channel.send(b"@i?"))
            await asyncio.sleep(0)
            frame = mgmt_frame(2, 1, b"OK a=1\r\nb=2\r\n")
            channel.on_indication(None, bytearray(frame[:16]))
            self.assertFalse(task.done())
            channel.on_indication(None, bytearray(frame[16:]))
        self.assertEqual(await task, b"OK a=1\r\nb=2\r\n")

    async def test_orphaned_response_header_is_ignored(self):
        channel = terminal.ManagementChannel(FakeWriteClient(), cfg(), 512)
        with contextlib.redirect_stderr(io.StringIO()) as errors:
            channel.on_indication(None, bytearray(b"garbage"))
        self.assertIn("orphaned", errors.getvalue())

    async def test_json_output_is_one_object_per_message_and_redacts_secrets(self):
        channel = terminal.ManagementChannel(FakeWriteClient(), cfg(), 512,
                                             json_output=True)
        buffer = io.StringIO()
        with self.quiet(), contextlib.redirect_stdout(buffer):
            task = asyncio.create_task(channel.send(b"@w=ssid,secret"))
            await asyncio.sleep(0)
            channel.on_indication(None, bytearray(
                mgmt_frame(2, 1, b"OK wifi=accepted\r\n")))
            await task
            channel.on_indication(None, bytearray(
                mgmt_frame(3, 9, b"OK event wifi result=0\r\n",
                           terminal.MGMT_FLAG_FINAL)))

        records = [json.loads(line) for line in buffer.getvalue().splitlines()]
        self.assertEqual(len(records), 2)
        self.assertEqual(records[0]["type"], "response")
        self.assertTrue(records[0]["ok"])
        self.assertEqual(records[0]["lines"], ["OK wifi=accepted"])
        self.assertEqual(records[0]["command"], "@w=<redacted>")
        self.assertEqual(records[1]["type"], "event")
        self.assertNotIn("secret", buffer.getvalue())

    async def test_oversized_command_is_rejected_before_any_write(self):
        client = FakeWriteClient()
        channel = terminal.ManagementChannel(client, cfg(), 8)
        with self.assertRaises(ValueError):
            await channel.send(b"@i?" + b"x" * 32)
        self.assertEqual(client.writes, [])


class ReliableUartTests(unittest.TestCase):
    def make(self, max_payload=232):
        received = []
        channel = terminal.ReliableUartChannel(
            FakeWriteClient(), cfg(), max_payload, 1, 1,
            lambda _c, data: received.append(bytes(data)),
        )
        return channel, received

    def test_frame_is_reassembled_and_delivered_once(self):
        channel, received = self.make()
        channel.on_indication(None, bytearray(uart_frame(1, b"hello")))
        self.assertEqual(received, [b"hello"])
        self.assertEqual(channel.rx_sequence, 2)

    def test_fragmented_frame_waits_for_the_rest(self):
        channel, received = self.make()
        frame = uart_frame(1, b"hello world")
        channel.on_indication(None, bytearray(frame[:14]))
        self.assertEqual(received, [])
        channel.on_indication(None, bytearray(frame[14:]))
        self.assertEqual(received, [b"hello world"])

    def test_duplicate_sequence_is_dropped_silently(self):
        channel, received = self.make()
        channel.on_indication(None, bytearray(uart_frame(1, b"one")))
        channel.on_indication(None, bytearray(uart_frame(1, b"one")))
        self.assertEqual(received, [b"one"])
        self.assertEqual(channel.rx_sequence, 2)

    def test_sequence_gap_is_reported_and_not_delivered(self):
        channel, received = self.make()
        with contextlib.redirect_stderr(io.StringIO()) as errors:
            channel.on_indication(None, bytearray(uart_frame(5, b"skipped")))
        self.assertEqual(received, [])
        self.assertEqual(channel.rx_sequence, 1)
        self.assertIn("sequence gap", errors.getvalue())

    def test_write_uses_one_sequence_per_frame(self):
        client = FakeWriteClient()
        received = []
        channel = terminal.ReliableUartChannel(
            client, cfg(write_size=20), 8, 1, 1,
            lambda _c, data: received.append(bytes(data)),
        )
        asyncio.run(channel.write(b"abcdefghijklmnopqr"))

        frames = []
        buffer = b""
        for _uuid, chunk in client.writes:
            buffer += chunk
            while len(buffer) >= terminal.RELIABLE_UART_HEADER.size:
                _m, _v, _f, sequence, expected, _r = \
                    terminal.RELIABLE_UART_HEADER.unpack_from(buffer)
                if len(buffer) < terminal.RELIABLE_UART_HEADER.size + expected:
                    break
                frames.append((sequence, buffer[terminal.RELIABLE_UART_HEADER.size:
                                               terminal.RELIABLE_UART_HEADER.size
                                               + expected]))
                buffer = buffer[terminal.RELIABLE_UART_HEADER.size + expected:]

        self.assertEqual([sequence for sequence, _ in frames], [1, 2, 3])
        self.assertEqual(b"".join(payload for _s, payload in frames),
                         b"abcdefghijklmnopqr")
        self.assertEqual(channel.tx_sequence, 4)


class BleWriteSizeTests(unittest.TestCase):
    class Services:
        def __init__(self, size):
            self.size = size

        def get_characteristic(self, _uuid):
            return types.SimpleNamespace(max_write_without_response_size=self.size)

    def configure(self, client, requested=0):
        config = cfg()
        with contextlib.redirect_stderr(io.StringIO()):
            terminal.configure_ble_write_size(client, config, requested)
        return config.write_size

    def test_manual_size_is_clamped(self):
        client = types.SimpleNamespace(services=None)
        self.assertEqual(self.configure(client, 300), 244)
        self.assertEqual(self.configure(client, 20), 20)

    def test_advertised_characteristic_size_wins(self):
        client = types.SimpleNamespace(services=self.Services(185), mtu_size=247)
        self.assertEqual(self.configure(client), 185)

    def test_mtu_fallback_when_the_characteristic_is_silent(self):
        client = types.SimpleNamespace(services=self.Services(0), mtu_size=247)
        self.assertEqual(self.configure(client), 244)

    def test_unknown_client_falls_back_to_a_safe_chunk(self):
        client = types.SimpleNamespace()
        self.assertEqual(self.configure(client), 20)


if __name__ == "__main__":
    unittest.main()
