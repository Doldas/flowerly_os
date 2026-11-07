#!/usr/bin/env python3
"""Required power-loss and slot-fallback test matrix, no fake PASS (#335)."""

SCENARIOS=("stage-complete","stage-interrupted","arm-before-rename","arm-after-rename",
           "boot-before-bless","unhealthy-b-fallback","data-survives","tampered-b-refused")

def validate_scenarios(results):
    if not isinstance(results,dict) or set(results)!=set(SCENARIOS):
        raise ValueError("missing or extra power-loss scenarios")
    if any(type(value) is not str or value not in ("PASS","FAIL","NOT_RUN","INCONCLUSIVE") for value in results.values()):
        raise ValueError("invalid scenario outcome")
    pending=sorted(k for k,v in results.items() if v!="PASS")
    return {"gate":"PASS" if not pending else "BLOCKED",
            "unverified":pending,"passed":len(SCENARIOS)-len(pending)}
