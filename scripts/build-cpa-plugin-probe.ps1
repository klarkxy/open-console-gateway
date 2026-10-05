param([Parameter(Mandatory=$true)][string]$OutputDirectory)
$ErrorActionPreference = 'Stop'
$probeBuildDir = [IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Force -Path $probeBuildDir | Out-Null
# cJSON is used only by this Windows experiment, never added to OCG's dependencies.
@'
import {writeFile} from 'node:fs/promises';
import {createHash} from 'node:crypto';
import {join} from 'node:path';
const dir=process.argv[2], records=[];
for(const path of ['cJSON.c','cJSON.h','LICENSE']) {
  const url='https://raw.githubusercontent.com/DaveGamble/cJSON/v1.7.19/'+path;
  const response=await fetch(url);if(!response.ok)throw Error(`${url}: ${response.status}`);
  const bytes=Buffer.from(await response.arrayBuffer());await writeFile(join(dir,path),bytes);
  records.push({path,url,sha256:createHash('sha256').update(bytes).digest('hex')});
}
await writeFile(join(dir,'dependencies.json'),JSON.stringify(records,null,2));
'@ | node --input-type=module - $probeBuildDir
if ($LASTEXITCODE -ne 0) { throw 'Dependency download failed' }
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'cpa-plugin-probe.c') -Destination (Join-Path $probeBuildDir 'probe.c')
$probeVcVars = 'C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat'
if (!(Test-Path -LiteralPath $probeVcVars)) { throw 'Visual Studio 2022 Build Tools required for this Windows probe' }
Set-Content -LiteralPath (Join-Path $probeBuildDir 'build.cmd') -Value @(
  '@echo off',
  ('call "{0}" >nul' -f $probeVcVars),
  'if errorlevel 1 exit /b 1',
  'cl /nologo /W4 /O2 /MT /LD /D_CRT_SECURE_NO_WARNINGS /DCJSON_HIDE_SYMBOLS probe.c cJSON.c /link /OUT:ocg-probe.dll'
)
Push-Location -LiteralPath $probeBuildDir
try { cmd /c build.cmd; if ($LASTEXITCODE -ne 0) { throw 'Probe build failed' } }
finally { Pop-Location }
Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $probeBuildDir 'ocg-probe.dll')
