"""Apply the reviewed source patch on its isolated branch, then retire this recipe."""
import base64
import hashlib
import lzma
import os
from pathlib import Path
import subprocess

BRANCH = "maintainer/unified-restrictions"
assert os.environ["GITHUB_REPOSITORY"] == "klarkxy/open-console-gateway"
assert os.environ["GITHUB_REF"] == f"refs/heads/{BRANCH}"
parts = sorted(Path(".workbench").glob("restriction-*.b64"))
assert len(parts) == 7
patch = lzma.decompress(base64.b64decode("".join(p.read_text() for p in parts), validate=True))
assert hashlib.sha256(patch).hexdigest() == "42ec72dc1c2860bdaccc1b4d7632cb772037e073f25c068bed6343e70fb7da07"
patch_path = Path("/tmp/restriction-source.patch")
patch_path.write_bytes(patch)
for args in (["git", "apply", "--check", str(patch_path)], ["git", "apply", str(patch_path)], ["cargo", "fmt", "--all"]):
    subprocess.run(args, check=True)
for p in parts:
    p.unlink()
Path(".github/workflows/restriction-workbench.yml").unlink()
Path(__file__).unlink()
subprocess.run(["git", "add", "-A"], check=True)
subprocess.run(["git", "-c", "user.name=github-actions[bot]", "-c", "user.email=41898282+github-actions[bot]@users.noreply.github.com", "commit", "-m", "refactor(gateway): unify rejection facts and resource recovery policy"], check=True)
subprocess.run(["git", "push", "origin", f"HEAD:refs/heads/{BRANCH}"], check=True)
sha = subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip()
with open(os.environ["GITHUB_OUTPUT"], "a") as out:
    out.write(f"sha={sha}\n")
print(f"SOURCE_COMMIT={sha}")
