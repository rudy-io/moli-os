#!/usr/bin/env python3
"""The Ollama Modelfile of a house model: Qwen3's template with the contract's tools written in it, BEFORE
the house's prompt (contract v4: the tools' reading stays cached when the house changes), as in training
(`qwen3_tools_first.jinja`). Ollama's own rendering of tools differs from the training's: they are frozen here.
    python3 make_modelfile.py --gguf ./moli-home-1.7b-v4-q4_k_m.gguf > Modelfile"""
import argparse
import json
from pathlib import Path

TOOLS = Path(__file__).resolve().parents[1] / "contract" / "tools.json"
HEAD = "# Tools\n\nYou may call one or more functions to assist with the user query.\n\nYou are provided with function signatures within <tools></tools> XML tags:\n<tools>"
TAIL = ("\n</tools>\n\nFor each function call, return a json object with function name and arguments within <tool_call></tool_call> XML tags:\n"
        "<tool_call>\n{\"name\": <function-name>, \"arguments\": <args-json-object>}\n</tool_call>")
# Qwen3's dialogue as Ollama's own Qwen3 template writes it (Ollama finds the thinking and tool tags
# there; an empty thinking block before the answer, as in training).
HEADER = '{{- $lastUserIdx := -1 -}}\n{{- range $idx, $msg := .Messages -}}\n{{- if eq $msg.Role "user" }}{{ $lastUserIdx = $idx }}{{ end -}}\n{{- end }}\n'
BODY = '{{- range $i, $_ := .Messages }}\n{{- $last := eq (len (slice $.Messages $i)) 1 -}}\n{{- if eq .Role "user" }}<|im_start|>user\n{{ .Content }}\n<|im_end|>\n{{ else if eq .Role "assistant" }}<|im_start|>assistant\n{{ if (and $.IsThinkSet (and .Thinking (or $last (gt $i $lastUserIdx)))) -}}\n<think>{{ .Thinking }}</think>\n{{ end -}}\n{{ if .Content }}{{ .Content }}\n{{- else if .ToolCalls }}<tool_call>\n{{ range .ToolCalls }}{"name": "{{ .Function.Name }}", "arguments": {{ .Function.Arguments }}}\n{{ end }}</tool_call>\n{{- end }}{{ if not $last }}<|im_end|>\n{{ end }}\n{{- else if eq .Role "tool" }}<|im_start|>user\n<tool_response>\n{{ .Content }}\n</tool_response><|im_end|>\n{{ end }}\n{{- if and (ne .Role "assistant") $last }}<|im_start|>assistant\n<think>\n\n</think>\n\n\n{{ end }}\n{{- end }}'


def main():
    a = argparse.ArgumentParser()
    a.add_argument("--gguf", required=True)
    args = a.parse_args()
    spec = json.loads(TOOLS.read_text(encoding="utf-8"))
    tools = "".join("\n" + json.dumps(t, ensure_ascii=False) for t in spec["tools"])
    system = "<|im_start|>system\n" + HEAD + tools + TAIL + "{{ if .System }}\n\n{{ .System }}{{ end }}<|im_end|>\n"
    print(f"# Modèle maison de Moli, contrat moli-v{spec['version']} : outils figés avant le prompt de la maison, comme à l'entraînement.")
    print(f"FROM {args.gguf}")
    # `.Tools` named once (it writes nothing): Ollama only lets a model take tools when its template does.
    print('TEMPLATE """' + HEADER + '{{- if .Tools }}{{ end }}' + system + BODY + '"""')
    print('PARAMETER stop "<|im_end|>"')
    print("PARAMETER temperature 0")
    print("PARAMETER num_ctx 8192")


if __name__ == "__main__":
    main()
