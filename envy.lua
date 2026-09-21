-- envy.lua - Project manifest
-- @envy schema "1"
-- @envy version "0.4.4"
-- @envy bin "bin"
-- @envy sha256sums "f32f09cb8ff05be571365ea203f3ba1bddcc1eabfc1af89cf3deb7c7e0a481ad"
-- @envy deploy "true"
-- @envy root "true"

BUNDLES = {
  ["first-party"] = {
    identity = "envy.package-specs@r5",
    source = "https://github.com/envy-package-manager/package-specs.git",
    ref = "cea307a81d8d3693e5261db318fcd0606f3b3ab3",
  },
}

PACKAGES = {
  {
    spec = "local.rust@r1",
    source = envy.abspath("tools/envy/rust.lua"),
    options = { version = "1.98.1" },
  },
  {
    spec = "envy.python@r2",
    bundle = "first-party",
    options = {
      version = "3.13.15",
      release = "20260825",
      provide_python3 = true,
    },
  },
  {
    spec = "envy.uv@r1",
    bundle = "first-party",
    options = { version = "0.12.8" },
  },
}
