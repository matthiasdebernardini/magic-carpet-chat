"""Jev judges one walkthrough step from the OCR text of the app window.

  uv run --with typesafe-sdk python judge.py <step-json>

stdin/argv: a JSON object {"step": str, "expected": str, "ocr": str,
"forbidden": [str]}. stdout: one JSON object with the Noul probabilities and
the Choice. Jev is text-only: it sees the OCR text, never the image, and it
judges; the driver still decides what to do with the answer.

Requires TYPESAFE_API_KEY in the environment.
"""
import json
import sys

from typesafe_sdk import Choice, Noul, RetryPolicy, TypeSafeClient


def judge(step: dict) -> dict:
    state = {
        "step": step["step"],
        "expected_screen": step["expected"],
        "forbidden_text": step.get("forbidden", []),
        "ocr_text": step["ocr"],
        "scope": (
            "The ocr_text is tesseract output from a screenshot of a desktop app "
            "window. OCR can drop, split or misspell words; a typographic "
            "apostrophe often reads as a straight one or a stray character. "
            "Judge the meaning of what is visible, not exact spelling."
        ),
    }
    with TypeSafeClient(retry=RetryPolicy(max_retries=2, timeout=30)) as client:
        result = client.system_one(
            state=state,
            questions={
                "expected_visible": Noul(
                    instructions=(
                        "Does `ocr_text` show the screen described in "
                        "`expected_screen`? Every element named in "
                        "`expected_screen` must have a recognisable trace in the "
                        "OCR text. A missing element is a no, not uncertainty."
                    )
                ),
                "loading_or_transitional": Noul(
                    instructions=(
                        "Does `ocr_text` show the app still loading or checking, "
                        "for example words like Checking, Loading, or an ellipsis "
                        "where `expected_screen` describes a settled result?"
                    )
                ),
                "forbidden_visible": Noul(
                    instructions=(
                        "Does `ocr_text` contain any phrase from "
                        "`forbidden_text`, allowing for OCR misspellings? An "
                        "empty `forbidden_text` list is a no."
                    )
                ),
                "next": Choice(
                    instructions=(
                        "Choose what the walkthrough driver should do after this "
                        "screenshot, using `expected_screen` and `ocr_text` only."
                    ),
                    criteria={
                        "proceed": "The expected screen is on display; move to the next step.",
                        "reobserve": "The screen is still loading or changing; take another screenshot before deciding.",
                        "abort": "The screen shows something other than the expected result, an error, or forbidden text; stop the take.",
                    },
                ),
            },
        )
    nouls = {k: result.answers[k].noul for k in
             ("expected_visible", "loading_or_transitional", "forbidden_visible")}
    choice = result.answers["next"]
    return {
        "step": step["step"],
        **nouls,
        "next": choice.choice,
        "next_confidence": choice.confidence,
    }


if __name__ == "__main__":
    raw = sys.argv[1] if len(sys.argv) > 1 else sys.stdin.read()
    print(json.dumps(judge(json.loads(raw))))
