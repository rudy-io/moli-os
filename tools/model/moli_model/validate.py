"""Every example must be one a correct Moli would give: real points, valid values, nothing the contract lacks."""
from . import houses

ARG_TOOLS = {"set", "get_device", "energy", "history", "rename_device", "move_device", "set_device_enabled",
             "rename_room", "add_person", "set_home_location", "refresh_weather", "request_driver"}


def _bool(v):
    return isinstance(v, bool)


def _number(v):
    return isinstance(v, (int, float)) and not isinstance(v, bool)


COLOURS = {"rouge", "red", "vert", "green", "bleu", "blue", "jaune", "yellow", "orange", "violet", "purple", "rose", "pink",
           "cyan", "magenta", "turquoise", "indigo", "ambre", "amber", "blanc", "white", "or", "blanc chaud", "warm white",
           "blanc froid", "cool white", "blanc neutre", "neutral white"}
LOOKS = {"feu", "bougies", "fin-de-journee", "cocooning", "fete", "ocean", "foret", "aurore", "romantique", "cinema",
         "lecture", "veilleuse"}
PROPOSING = {"rename_device", "move_device", "set_device_enabled", "rename_room", "add_person", "set_home_location", "undo"}
MAX_CALLS = 4


def validate(case, house, tools):
    answer = case["answer"]
    if "calls" in answer:
        calls = answer["calls"]
        problems = [] if 1 <= len(calls) <= MAX_CALLS else [f"{len(calls)} calls: 1 to {MAX_CALLS}"]
        if len(calls) > 1 and any(c["tool"] in PROPOSING for c in calls):
            problems.append("a change of configuration is proposed alone")
        for call in calls:
            problems += validate({**case, "answer": call}, house, tools)
        return problems
    problems = []
    by_name = {t["function"]["name"]: t["function"]["parameters"] for t in tools}
    devs = {d["id"]: d for d in houses.devices(house)}
    if "text" in answer:
        if not answer["text"].strip():
            problems.append("empty text answer")
        if case["cat"] == "protégée" and "protégée" not in answer["text"]:
            problems.append("a refusal must say the room is protégée")
        if case["cat"] == "ambigu" and not answer["text"].rstrip().endswith("?"):
            problems.append("an ambiguity is answered by a question")
        return problems
    tool, args = answer["tool"], answer.get("arguments", {})
    if tool not in by_name:
        return [f"unknown tool {tool}"]
    schema = by_name[tool]
    for required in schema.get("required", []):
        if required not in args:
            problems.append(f"{tool}: missing {required}")
    for key in args:
        if key not in schema["properties"]:
            problems.append(f"{tool}: unexpected argument {key}")
            continue
        prop = schema["properties"][key]
        if "enum" in prop and args[key] not in prop["enum"]:
            problems.append(f"{tool}: {key} must be one of {prop['enum']}")
        if prop.get("type") == "integer":
            v = args[key]
            if not isinstance(v, int) or isinstance(v, bool) or v < prop.get("minimum", v) or v > prop.get("maximum", v):
                problems.append(f"{tool}: {key} out of range")
    problems += _v4(tool, args, house, devs)
    if tool == "set":
        point = str(args.get("point", ""))
        dev_id, _, key = point.partition("/")
        dev = devs.get(dev_id)
        spec = houses.point_spec(dev, key) if dev else None
        if not dev or not spec:
            problems.append(f"set: unknown point {point}")
        else:
            if not spec["access"]["write"]:
                problems.append(f"set: {point} is read-only")
            if dev["room"] in house["protected"]:
                problems.append(f"set: {dev['room']} est protégée")
            problems += _value(spec, args.get("value"), point)
    elif "device" in args and args["device"] not in devs:
        problems.append(f"{tool}: unknown device {args['device']}")
    elif tool in ("rename_room", "move_device"):
        rooms = {r["name"] for r in house["layout"]["rooms"]}
        if args.get("room") not in rooms:
            problems.append(f"{tool}: unknown room {args.get('room')}")
    elif tool == "delegate":
        question = str(args.get("question", ""))
        if not question.strip() or len(question) > 600:
            problems.append("delegate: the question is empty or too long")
        elif case["cat"] == "hors_cadre" and question != case["user"]:
            problems.append("delegate: the question is copied word for word, not rewritten")
    elif tool == "history":
        dev_id = str(args.get("point", "")).partition("/")[0]
        if dev_id not in devs:
            problems.append(f"history: unknown device {dev_id}")
        if "hours" in args and not _number(args["hours"]):
            problems.append("history: hours must be a number")
    elif tool == "energy" and "step" in args and args["step"] not in ("hour", "day", "month"):
        problems.append("energy: bad step")
    return problems


def _writable(devs, point):
    dev_id, _, key = str(point).partition("/")
    dev = devs.get(dev_id)
    spec = houses.point_spec(dev, key) if dev else None
    return dev, spec if spec and spec["access"]["write"] else None


def _v4(tool, args, house, devs):
    """The contract v4 tools: what Moli would refuse is never taught."""
    rooms = {r["name"] for r in house["layout"]["rooms"]}
    problems = []
    if "room" in args and tool in ("set_all", "find", "ambiance") and args["room"] not in rooms:
        problems.append(f"{tool}: unknown room {args['room']}")
    if tool in ("adjust", "schedule"):
        dev, spec = _writable(devs, args.get("point"))
        if not spec:
            return problems + [f"{tool}: {args.get('point')} is not a writable point"]
        if dev["room"] in house["protected"]:
            problems.append(f"{tool}: {dev['room']} est protégée")
        if tool == "adjust" and (spec["kind"]["type"] != "numeric" or not _number(args.get("by")) or args.get("by") == 0):
            problems.append("adjust: a number moved by a non-zero number")
        if tool == "schedule":
            problems += _value(spec, args.get("value"), args.get("point"))
    if tool == "remind":
        text = str(args.get("text", "")).strip()
        if not text or len(text) > 120:
            problems.append("remind: a short text")
    if tool == "draft_automation" and not str(args.get("description", "")).strip():
        problems.append("draft_automation: empty")
    if tool == "ambiance":
        look, colour = args.get("look"), args.get("color")
        if (look is None) == (colour is None) or (look is not None and look not in LOOKS) or (colour is not None and colour not in COLOURS):
            problems.append("ambiance: one known look or one colour name")
    if tool == "set" and str(args.get("point", "")).endswith("/color"):
        if args.get("value") not in COLOURS:
            problems.append("set: a colour is said by its name")
    return problems


def _value(spec, value, point):
    kind = spec["kind"]
    if kind["type"] == "binary":
        return [] if _bool(value) else [f"set: {point} takes a boolean"]
    if kind["type"] == "numeric":
        if not _number(value):
            return [f"set: {point} takes a number"]
        lo, hi = kind.get("min"), kind.get("max")
        if (lo is not None and value < lo) or (hi is not None and value > hi):
            return [f"set: {point} out of bounds {lo}-{hi}"]
        return []
    if kind["type"] == "enum":
        return [] if value in kind["values"] else [f"set: {point} must be one of {kind['values']}"]
    return [] if isinstance(value, str) else [f"set: {point} takes text"]
