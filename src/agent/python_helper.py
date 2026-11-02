"""Calling Sense Sim's tools from a script run by the assistant.

    import sensesim
    scene = sensesim.call("get_scene")
    sensesim.call("update_object", name="Shelf_3", position=[1, 0, 2])

Results come back as parsed JSON when they are JSON, and as text otherwise. A refused or failed
call raises sensesim.ToolError. Each call passes the same approvals as any other.
"""

import json
import os
import sys

_PREFIX = "\x1e" + os.environ.get("SENSESIM_RPC_PREFIX", "") + " "


class ToolError(Exception):
    """The app refused the call, or the tool failed."""


def call(tool, **arguments):
    """Runs one of the app's tools, and waits for its answer."""
    # The app reads output a line at a time: anything printed before this has to go first.
    sys.stdout.flush()
    sys.stderr.flush()
    sys.__stdout__.write(_PREFIX + json.dumps({"tool": tool, "input": arguments}) + "\n")
    sys.__stdout__.flush()
    line = sys.__stdin__.readline()
    if not line:
        raise ToolError("the app stopped answering")
    reply = json.loads(line)
    if reply.get("is_error"):
        raise ToolError(reply.get("text", ""))
    text = reply.get("text", "")
    try:
        return json.loads(text)
    except ValueError:
        return text
