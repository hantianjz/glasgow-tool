import hashlib
import json
from pathlib import Path
import sys
import unittest

GATEWARE = Path(__file__).resolve().parents[1]
ROOT = GATEWARE.parent
sys.path.insert(0, str(GATEWARE))

import build_uart


class ResourceTests(unittest.TestCase):
    def test_generated_resources_cover_exact_rev_c_profile(self):
        output = ROOT / "target" / "glasgow-uart"
        manifests = sorted(path.stem for path in output.glob("C*.json"))
        self.assertEqual(manifests, ["C0", "C1", "C2", "C3"])
        for revision in manifests:
            manifest = json.loads((output / f"{revision}.json").read_text())
            bitstream = (output / manifest["bitstream"]["file"]).read_bytes()
            self.assertEqual(manifest["schema_version"], 1)
            self.assertEqual(manifest["upstream_commit"], build_uart.UPSTREAM_COMMIT)
            self.assertEqual(manifest["revision"], revision)
            self.assertEqual(
                manifest["profile"],
                build_uart.PROFILE,
            )
            self.assertEqual(
                hashlib.sha256(bitstream).hexdigest(),
                manifest["bitstream"]["sha256"],
            )
            self.assertEqual(manifest["pipe"]["rx"]["endpoint"], 0x86)
            self.assertEqual(manifest["pipe"]["tx"]["endpoint"], 0x02)

    def test_firmware_descriptors_derive_api9_high_speed_pipe(self):
        segments, _source = build_uart.firmware_segments()
        descriptors = build_uart.parse_usb_config(segments)
        interfaces = {(item["interface"], item["alternate_setting"]) for item in descriptors}
        self.assertIn((1, 2), interfaces)
        self.assertIn((3, 2), interfaces)
        endpoints = {
            endpoint["endpoint"]
            for item in descriptors
            for endpoint in item["endpoints"]
            if item["alternate_setting"] == 2
        }
        self.assertEqual(endpoints, {0x02, 0x86})


if __name__ == "__main__":
    unittest.main()
