-- @envy schema "1"
IDENTITY = "local.rust@r1"
EXPORTABLE = true

local releases = {
  ["1.98.1"] = {
    date = "2026-09-03",
    hashes = {
      ["aarch64-apple-darwin"] =
        "8f7c9de34c12c66dc55a6dbdd63b2b9645d4418775d4b975b0078a602f66e3e8",
      ["x86_64-apple-darwin"] =
        "0d5aea6011d3b553e5aa71580b0cd7f9f0614eef17f7e5767e6bc161541ed85f",
      ["aarch64-unknown-linux-gnu"] =
        "0b514a8cc1cbcd939bff0f151661fe58b6ea5c7a7f645a5098c69e32e8c1e0a2",
      ["x86_64-unknown-linux-gnu"] =
        "5326b36c53de11d148c8f8dab6553a3d1006c2cfd32123683073fad3c302605b",
      ["aarch64-pc-windows-msvc"] =
        "a9e186a281308cdf876ab6a0e2c37d75ef43e1c466a44641d301120e0f3358c5",
      ["x86_64-pc-windows-msvc"] =
        "c34c3f01633efe5edac0e1ed1ee66d5ec13a7f27e57c149059ed6bdbe0534407",
    },
  },
}

local function target()
  if envy.PLATFORM == "darwin" then
    return (envy.ARCH == "arm64") and "aarch64-apple-darwin"
      or "x86_64-apple-darwin"
  elseif envy.PLATFORM == "linux" then
    return (envy.ARCH == "arm64") and "aarch64-unknown-linux-gnu"
      or "x86_64-unknown-linux-gnu"
  elseif envy.PLATFORM == "windows" then
    return (envy.ARCH == "arm64") and "aarch64-pc-windows-msvc"
      or "x86_64-pc-windows-msvc"
  end
  error("unsupported platform: " .. envy.PLATFORM)
end

OPTIONS = function(opts)
  envy.options({
    version = { required = true, choices = { "1.98.1" } },
  })
end

FETCH = function(tmp_dir, opts)
  local release = releases[opts.version]
  local host = target()
  return {
    source = "https://static.rust-lang.org/dist/" .. release.date
      .. "/rust-" .. opts.version .. "-" .. host .. ".tar.xz",
    sha256 = release.hashes[host],
  }
end

STAGE = { strip = 1 }

local function sh_quote(value)
  return "'" .. value:gsub("'", "'\\''") .. "'"
end

local function ps_quote(value)
  return "'" .. value:gsub("'", "''") .. "'"
end

local function selected_components()
  return table.concat({
    "rustc",
    "rust-std-" .. target(),
    "cargo",
    "rustfmt-preview",
    "clippy-preview",
    "rust-analyzer-preview",
  }, ",")
end

INSTALL = function(install_dir, stage_dir)
  if envy.PLATFORM ~= "windows" then
    envy.run(
      "./install.sh --prefix=" .. sh_quote(install_dir)
        .. " --disable-ldconfig --components=" .. selected_components(),
      { cwd = stage_dir })
    return
  end

  local components = {}
  for component in selected_components():gmatch("[^,]+") do
    table.insert(components, "'" .. component .. "'")
  end

  -- Rust's standalone archive only ships a POSIX installer. On Windows,
  -- apply the same component manifests with PowerShell into envy's package
  -- directory rather than mutating Program Files or the user's rustup state.
  local script = [[
$stage = ]] .. ps_quote(stage_dir) .. [[
$destination = ]] .. ps_quote(install_dir) .. [[
$components = @(]] .. table.concat(components, ", ") .. [[)
foreach ($component in $components) {
  $componentRoot = Join-Path $stage $component
  Get-Content (Join-Path $componentRoot 'manifest.in') | ForEach-Object {
    $entry = $_ -split ':', 2
    $source = Join-Path $componentRoot $entry[1]
    $target = Join-Path $destination $entry[1]
    $parent = Split-Path -Parent $target
    if ($parent) {
      New-Item -ItemType Directory -Force -Path $parent | Out-Null
    }
    if ($entry[0] -eq 'dir') {
      if (Test-Path $target) { Remove-Item -Recurse -Force $target }
      Copy-Item -Recurse -Force $source $target
    } else {
      Copy-Item -Force $source $target
    }
  }
}
]]
  envy.run(script, { cwd = stage_dir, shell = ENVY_SHELL.POWERSHELL })
end

local bin = "bin/"
PRODUCTS = {
  cargo = bin .. "cargo" .. envy.EXE_EXT,
  ["cargo-clippy"] = bin .. "cargo-clippy" .. envy.EXE_EXT,
  ["cargo-fmt"] = bin .. "cargo-fmt" .. envy.EXE_EXT,
  ["clippy-driver"] = bin .. "clippy-driver" .. envy.EXE_EXT,
  rustc = bin .. "rustc" .. envy.EXE_EXT,
  rustdoc = bin .. "rustdoc" .. envy.EXE_EXT,
  rustfmt = bin .. "rustfmt" .. envy.EXE_EXT,
  ["rust-analyzer"] = bin .. "rust-analyzer" .. envy.EXE_EXT,
}
