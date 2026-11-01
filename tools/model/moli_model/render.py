"""The prompts, rendered by Moli's own code (`moli-os model …`)."""
import json
import subprocess


def render_prompt(binary, house):
    """`(prompt, aliases)` for `house`, as Moli would send them: the system prompt, and which real device
    each number in it (`d1`…) stands for. Generator-only keys are dropped."""
    payload = {k: house[k] for k in ("layout", "protected", "devices")}
    done = subprocess.run([binary, "model", "prompt"], input=json.dumps(payload, ensure_ascii=False),
                          capture_output=True, text=True, check=False)
    if done.returncode != 0:
        raise RuntimeError(f"moli-os model prompt failed: {done.stderr.strip()[:300]}")
    rendered = json.loads(done.stdout)
    return rendered["prompt"], rendered["aliases"]


def render_tools(binary):
    done = subprocess.run([binary, "model", "tools"], capture_output=True, text=True, check=True)
    return json.loads(done.stdout)
