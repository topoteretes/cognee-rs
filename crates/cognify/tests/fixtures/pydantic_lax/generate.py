#!/usr/bin/env python3
"""Regenerate `pydantic_lax_table.json`, the ground truth for the Pydantic
lax-mode coercions in `crates/cognify/src/dynamic_graph.rs`.

It builds the model exactly as cognee's `graph_schema_to_graph_model` does
(datamodel-code-generator, Pydantic v2, `DataPoint` base, cognee @ f154b32),
validates one JSON answer per row with `model_validate_json` -- the call
instructor makes for a structured-output answer -- and records what Pydantic
accepts, the value it holds (in the JSON form cognee's `JSONEncoder` writes to
storage), its `str()`, and the node id cognee derives from it:
`uuid5(NAMESPACE_OID, "Thing:" + _normalize_identity_value(value))`.

Run with a Python that has `pydantic` and `datamodel-code-generator`:

    python3 generate.py > pydantic_lax_table.json

The Rust test never derives an expectation from Rust code; it replays this file.
"""

import datetime
import enum
import json
import math
import re
import sys
import types
import warnings
from typing import cast
from uuid import NAMESPACE_OID, UUID, uuid5

import pydantic
from datamodel_code_generator import DataModelType, GenerateConfig, InputFileType, generate
from pydantic import BaseModel, ConfigDict

warnings.filterwarnings("ignore")


# Stand-in for cognee.infrastructure.engine.DataPoint: the only model_config it
# sets is arbitrary_types_allowed, which does not affect scalar validation.
class DataPoint(BaseModel):
    model_config = ConfigDict(arbitrary_types_allowed=True)


_engine = types.ModuleType("cognee.infrastructure.engine")
_engine.DataPoint = DataPoint
for _name, _module in (
    ("cognee", types.ModuleType("cognee")),
    ("cognee.infrastructure", types.ModuleType("cognee.infrastructure")),
    ("cognee.infrastructure.engine", _engine),
):
    sys.modules.setdefault(_name, _module)


def graph_schema_to_graph_model(schema):
    """cognee.shared.graph_model_utils.graph_schema_to_graph_model."""
    config = GenerateConfig(
        allow_remote_refs=False,
        input_file_type=InputFileType.JsonSchema,
        input_filename="dynamic.json",
        output_model_type=DataModelType.PydanticV2BaseModel,
        additional_imports=["cognee.infrastructure.engine.DataPoint", "typing.Any", "typing"],
        base_class="cognee.infrastructure.engine.DataPoint",
        type_overrides={"DataPoint": "cognee.infrastructure.engine.DataPoint"},
    )
    result = cast(str, generate(schema, config=config))
    result = re.sub(r"class DataPointModel\(DataPoint\):.*?(?=\nclass|\Z)", "", result, flags=re.DOTALL)
    result = result.replace("DataPointModel", "DataPoint")
    module = types.ModuleType("cognee.shared._generated_graph_models")
    sys.modules[module.__name__] = module
    exec(result, module.__dict__)
    namespace = module.__dict__
    model = namespace[schema["title"]]
    namespace["DataPoint"].model_rebuild()
    model.model_rebuild(_types_namespace=namespace)
    return model


DEFS = {
    "E": {"enum": ["x", "y"], "type": "string"},
    "EI": {"enum": [1, 2], "type": "integer"},
    "EB": {"enum": [True, False]},
    "EN": {"enum": [1.5, 2.5], "type": "number"},
    "UInt": {"enum": [1, 2]},
    "UFlt": {"enum": [1.5, 2.5]},
    "UMix": {"enum": ["a", 1, True]},
    "UStr1": {"enum": ["1", "2"]},
    "NInt": {"enum": [1, 2], "type": "number"},
    "SInt": {"enum": ["1", "2"], "type": "string"},
    "Txt": {"type": "string"},
    "Num": {"type": "number"},
}
REF = lambda name: {"$ref": f"#/$defs/{name}"}
DATE = {"type": "string", "format": "date"}
DATETIME = {"type": "string", "format": "date-time"}
TIME = {"type": "string", "format": "time"}
UUID_ = {"type": "string", "format": "uuid"}
NUMBER = {"type": "number"}
FIELDS = {
    "s": {"type": "string"},
    "n": {"type": "number"},
    "i": {"type": "integer"},
    "b": {"type": "boolean"},
    "e": REF("E"),
    "ei": REF("EI"),
    "eb": REF("EB"),
    "en": REF("EN"),
    "es": {"type": "string", "enum": ["a", "b"]},
    "uint": REF("UInt"),
    "uflt": REF("UFlt"),
    "umix": REF("UMix"),
    "ustr1": REF("UStr1"),
    "nint": REF("NInt"),
    "sint": REF("SInt"),
    "dt": DATETIME,
    "d": DATE,
    "t": TIME,
    "u": UUID_,
    "un_str_num": {"anyOf": [{"type": "string"}, {"type": "number"}]},
    "un_num_int": {"anyOf": [{"type": "number"}, {"type": "integer"}]},
    "un_int_num": {"anyOf": [{"type": "integer"}, {"type": "number"}]},
    "un_bool_int": {"anyOf": [{"type": "boolean"}, {"type": "integer"}]},
    "un_int_bool": {"anyOf": [{"type": "integer"}, {"type": "boolean"}]},
    "un_date_int": {"anyOf": [DATE, {"type": "integer"}]},
    "un_int_dt": {"anyOf": [{"type": "integer"}, DATETIME]},
    "opt_num": {"anyOf": [{"type": "number"}, {"type": "null"}]},
    "l_num": {"type": "array", "items": {"type": "number"}},
    "l_int": {"type": "array", "items": {"type": "integer"}},
    "l_date": {"type": "array", "items": DATE},
    "l_bool": {"type": "array", "items": {"type": "boolean"}},
    # Smart-mode unions pick the most exact variant (exact > strict > lax),
    # the first on a tie: an enum is always lax, a RootModel at best strict,
    # a JSON string strict for `str`/date/time/date-time but exact for UUID.
    "un_num_ustr1": {"anyOf": [NUMBER, REF("UStr1")]},
    "un_num_eb": {"anyOf": [NUMBER, REF("EB")]},
    "un_num_txt": {"anyOf": [NUMBER, REF("Txt")]},
    "un_str_txt": {"anyOf": [{"type": "string"}, REF("Txt")]},
    "un_txt_str": {"anyOf": [REF("Txt"), {"type": "string"}]},
    "un_int_numdef": {"anyOf": [{"type": "integer"}, REF("Num")]},
    "un_ei_int": {"anyOf": [REF("EI"), {"type": "integer"}]},
    "un_uint_bool": {"anyOf": [REF("UInt"), {"type": "boolean"}]},
    "un_bool_uint": {"anyOf": [{"type": "boolean"}, REF("UInt")]},
    "un_str_uuid": {"anyOf": [{"type": "string"}, UUID_]},
    "un_uuid_str": {"anyOf": [UUID_, {"type": "string"}]},
    "un_dt_num": {"anyOf": [DATETIME, NUMBER]},
    "un_num_dt": {"anyOf": [NUMBER, DATETIME]},
    "un_date_num": {"anyOf": [DATE, NUMBER]},
    "un_num_date": {"anyOf": [NUMBER, DATE]},
    "un_time_num": {"anyOf": [TIME, NUMBER]},
    "un_num_time": {"anyOf": [NUMBER, TIME]},
    "un_date_str": {"anyOf": [DATE, {"type": "string"}]},
    "un_str_dt": {"anyOf": [{"type": "string"}, DATETIME]},
    "un_int_date": {"anyOf": [{"type": "integer"}, DATE]},
    "l_dt": {"type": "array", "items": DATETIME},
    "l_time": {"type": "array", "items": TIME},
    "l_uuid": {"type": "array", "items": UUID_},
}

# Every input is raw JSON text, so `1.0`, `1e2` and big integers keep their
# spelling exactly as an LLM would emit them.
CASES = {
    "s": ['"a"', "1", "1.5", "true", "null", "[]"],
    "n": [
        '3', '3.5', '"3"', '"3.5"', '" 3.5 "', '"1e3"', '"1_0"', '"inf"', '"nan"', '"abc"', '""', "true", "false",
        "null", "[]", '"Infinity"', '"-inf"', '"+1.5"', '".5"', '"5."', '"1E3"', '"1__0"', '"_1"', '"1_"',
        '"0x10"', '"1,5"', '"\\u00a03\\u00a0"', '"3\\n"', '"  "', '"1e400"', '"-0"', '"1_0.5_5"', '"1_e5"',
        '"1.e5"', '".e5"', '"+.5"', '"-.5e-2"', '"1e-400"', '"1f"', '"1 0"', '"\\uff13"', '"\\uff11.\\uff15"',
    ],
    "i": [
        '3', '3.0', '3.5', '"3"', '" 3 "', '"+3"', '"3.0"', '"3.5"', '"1e3"', '"1_0"', '"abc"', '""', "true",
        "false", "1e20", "null", '"-3"', '"3 "', '"3_000"', '"0003"', '"-0"', '9223372036854775807',
        '-9223372036854775808', '1e18', '9.3e18', '"3.00"', '"3."', '".0"', '"3e0"', '"1.0e1"',
        '3.0000000000000004', '"\\u0663"', '"\\uff13"', '"3.50"', '"3.0_0"', '"-3.0"', '"+3.0"', '"1_0.0"',
        '"0.0"', '"-0.0"', '"1_000."', '"0_3"', '"3__0"', '"--3"', '"3.0.0"', '1.0e0', '-1e18', '"1.5e0"',
        '"99999999999999999999"', '9223372036854775808', '18446744073709551615', '18446744073709551616',
        '-9223372036854775809',
    ],
    "b": [
        "true", "false", "1", "0", "2", "1.0", "0.0", "1.5", "-1", '"true"', '"True"', '"TRUE"', '"yes"', '"Yes"',
        '"y"', '"t"', '"T"', '"on"', '"off"', '"OFF"', '"n"', '"no"', '"f"', '"false"', '"False"', '"1"', '"0"',
        '"1.0"', '" true "', '""', '"2"', '"maybe"', '"nope"', "[]", "null",
    ],
    "e": ['"x"', '"z"', "1", "true", "null"],
    "es": ['"a"', '"c"'],
    "ei": ["1", "1.0", '"1"', "2", "3", "true", '"x"', "1.5"],
    "eb": ["true", "false", "1", "0", '"true"', '"True"', "2"],
    "en": ["1.5", "2.5", '"1.5"', "1", "2"],
    "uint": ["1", "1.0", '"1"', "true", "3", '"x"'],
    "uflt": ["1.5", '"1.5"', "1"],
    "umix": ['"a"', "1", "true", "1.0", '"1"', "0"],
    "ustr1": ['"1"', "1"],
    "nint": ["1", "1.0", '"1"', "true", "1.5"],
    "sint": ['"1"', "1"],
    "dt": [
        '"2024-01-02T03:04:05Z"', '"2024-01-02T03:04:05+02:00"', '"2024-01-02T03:04:05"', '"2024-01-02"',
        "1700000000", "1700000000.5", '"1700000000"', '"1700000000.5"', "1700000000000", '"1700000000000"', "-1",
        '"-1"', "0", '"0"', "true", '"abc"', "253402300800", "253402300799", "1e20", '"2024-01-02T03:04:05.5Z"',
        '"2024-01-02 03:04:05Z"', '"2024-01-02T03:04Z"', '"1700000000 "', '" 1700000000"', '"+1700000000"',
        '"1_700_000_000"', '"1e9"', '"1.7e9"', "1.7e9", "19999999999", "20000000001", "20000000000",
        "-20000000001", "1700000000.123456789", '"1700000000.1234567"', '"1700000000."', '".5"', '"-0.5"', "-0.5",
        "253402300799000", "253402300800000", "-62135596800", "-62135596801", "1e15",
        '"2024-01-02T03:04:05+0200"', '"2024-01-02T03:04:05-02"', '"2024-01-02T03:04:05.1234567Z"', "-1.25",
        "-1.5", "0.9999999", "1.9999999", "1700000000123.7", "1700000000123.456", "-1700000000123.7",
        "-1700000000123", "-20000000000.5", "20000000000.5", '"20000000000.5"', '"-1.25"', '"1.5e1"', '"1.e1"',
        '"1.5E3"', '"inf.0"', '"+.5"', '"1.5.5"', '"1.5e"', '"0.0"', '"-0"', '"+0"', '"00000000000001"',
        '"9223372036854775807"', '"9223372036854775808"', "9223372036854775807", '"1700000000.123456"',
        '"1700000000.9999999"', '"0001-01-01T00:00:00Z"', '"0000-01-01T00:00:00Z"',
        '"9999-12-31T23:59:59.999999Z"', '"2024-01-02T03:04:05-00:00"', '"2024-01-02T03:04:05+23:59"',
        '"2024-01-02T03:04:05+24:00"', '"2024-01-02T24:00:00Z"', '"2024-02-29T00:00:00Z"',
        '"2023-02-29T00:00:00Z"', "-9223372036854775808", '"-9223372036854775808"', "-9223372036854775807",
    ],
    "d": [
        '"2024-01-02"', '"2024-01-02T00:00:00"', '"2024-01-02T00:00:00Z"', '"2024-01-02T10:00:00"', '"2024-1-2"',
        "1700000000", "1700000000000", '"1700000000"', '"1700000000.5"', "0", "-1", "86400", "86400.0", "86400.5",
        '"20240102"', "true", '"abc"', '"2024-02-30"', '"2024-01-02T00:00:00.000"', '"2024-01-02 00:00:00"',
        '"2024-01-02T00:00:00+02:00"', '"2024-01-02T00:00:00.5"', "-86400", "86400000", "86400000.0",
        "1700006400", "172800", '"172800"', '"1970-01-02"', '"0000-01-01"', '"9999-12-31"', '"2024-01-02 "',
        '"+2024-01-02"', "20000000000", "1.7e9", "253402214400", "253402300800", '"2024-W01-1"', '"86400.0"',
        '"86400.5"', '"-86400"', '"+86400"', '"86400."', '"2024-01-02T00:00:00.0"', '"2024-01-02t00:00:00"',
        '"2024-01-02T00:00Z"', '"2024-01-02T00:00:00.999999"', '"2024-01-02T00:00:00.0000001"', "-86400.0",
        "86400.00", "172800.0", "1e5", '"1e5"', '"1.728e5"', '"1.728E5"', "1.728e5", '"2024-01-02T24:00:00"',
        '"1969-12-31"', "-9223372036854775808", '"-9223372036854775808"',
    ],
    "t": [
        '"03:04:05"', '"03:04"', '"03:04:05.123456"', '"03:04:05.1234567"', '"03:04:05Z"', '"03:04:05+02:00"',
        '"2024-01-02T03:04:05"', '"24:00:00"', '"03:04:60"', "3661", "3661.5", '"3661"', "86400", "86399", "-1", "0",
        "true", '"abc"', '"3:04:05"', '"03:04:05 "', '"03:04:05-0200"', '"03:04:05.5"', '"03:04:05,5"',
        '"03:04:05z"', '"230405"', '"T03:04:05"', '"03:04:05+24:00"', '"23:59:59.9999999"', '"23:59:60"',
        '"00:00"', '"0:0:0"', "86399.999999", "86399.9999999", "86400.0", "1.5", "-0.5", "20000000000",
        '"12:00 PM"', '"03:04:05.0000009"', '"03:04:05.9999995"', '"03:04:05+02"', '"03:04:05 +02:00"',
        '"03:04+02:00"', '"03:04Z"', '"03:04:05.Z"', '"03:04:05."', '"24:00"', '"23:60"', '"-03:04"',
        '"03:04:05-00:00"', '"03:04:05-24:00"', '"03:04:05+23:59"', '"03:04:05+02:60"',
        '"03:04:05\\u221202:00"', "0.0", "86399.5", "1e0", '"00:00:00.000000"', "1.0000004", "1.0000005",
        "0.9999995", "0.9999994", "3661.123456789", "86399.9999995",
        "-9223372036854775808", '"-9223372036854775808"',
    ],
    "u": [
        '"12345678-1234-5678-1234-567812345678"', '"12345678123456781234567812345678"',
        '"{12345678-1234-5678-1234-567812345678}"', '"urn:uuid:12345678-1234-5678-1234-567812345678"',
        '"12345678-1234-5678-1234-56781234567"', '"ABCDEF78-1234-5678-1234-567812345678"', '"abc"', "1", "true",
        '"12345678-1234-5678-1234-56781234567g"', '"12345678-1234-5678-1234-567812345678 "',
        '"1234567812345678-1234-567812345678"', '"12345678-1234-5678-1234-5678123456789"',
        '"urn:uuid:ABCDEF78123456781234567812345678"', '"{12345678123456781234567812345678}"',
        '"12345678-1234-5678-1234-567812345678}"', '"UUID:12345678-1234-5678-1234-567812345678"',
        '"1234567-81234-5678-1234-567812345678"', '""', '"\\uff11\\uff12345678-1234-5678-1234-567812345678"',
    ],
    "un_str_num": ["3", '"3"', "3.5", "true"],
    "un_num_int": ["3", '"3"', "3.5", '"3.5"', "true"],
    "un_int_num": ["3", '"3"', "3.5", '"3.5"', "3.0", "true"],
    "un_bool_int": ["true", "1", '"1"', '"true"', "2"],
    "un_int_bool": ["true", "1", '"1"', '"true"', "2"],
    "un_date_int": ['"2024-01-02"', "86400", '"86400"', "5"],
    "un_int_dt": ["3", '"2024-01-02T03:04:05Z"', '"3"', "1700000000"],
    "opt_num": ["null", '"3"', "3"],
    "l_num": ['[1, "2", 3.5, true]', '["x"]'],
    "l_int": ['[1, "2", 3.0, true]', "[3.5]"],
    "l_date": ['["2024-01-02", 86400]', "[1700000000]"],
    "l_bool": ['[true, 1, "yes", 0]', "[2]"],
    "un_num_ustr1": ['"1"', "1", '"2"', '"x"'],
    "un_num_eb": ["true", "false", "1"],
    "un_num_txt": ['"1"', "1", '"x"'],
    "un_str_txt": ['"a"'],
    "un_txt_str": ['"a"'],
    "un_int_numdef": ["1", "1.5", '"1"'],
    "un_ei_int": ["1", '"1"'],
    "un_uint_bool": ["true", "1"],
    "un_bool_uint": ["true", "1"],
    "un_str_uuid": ['"12345678-1234-5678-1234-567812345678"', '"ABCDEF78-1234-5678-1234-567812345678"', '"abc"'],
    "un_uuid_str": ['"12345678123456781234567812345678"', '"abc"'],
    "un_dt_num": ["1700000000", "1700000000.5", '"1700000000"', '"2024-01-02T03:04:05Z"', '"3.5"', "true"],
    "un_num_dt": ["1700000000", "1700000000.5", '"1700000000"', '"2024-01-02T03:04:05Z"', '"3.5"', "true"],
    "un_date_num": ["86400", "86400.0", '"86400"', '"86400.0"', '"2024-01-02"', '"2024-01-02T00:00:00"'],
    "un_num_date": ["86400", "86400.0", '"86400"', '"86400.0"', '"-86400"', '"+86400"','"2024-01-02"', '"2024-01-02T00:00:00"'],
    "un_time_num": ["3661", "3661.5", '"3661"', '"03:04:05"'],
    "un_num_time": ["3661", "3661.5", '"3661"', '"03:04:05"'],
    "un_date_str": ['"2024-01-02"', '"2024-01-02T00:00:00"', '"86400"'],
    "un_str_dt": ['"2024-01-02T03:04:05Z"', '"1700000000"'],
    "un_int_date": ['"86400"', "86400", '"2024-01-02"'],
    "l_dt": [
        '["2024-01-02T03:04:05Z"]', '["2024-01-02T03:04:05+02:00", "2024-01-02T03:04:05.5-05:30"]',
        '["2024-01-02T03:04:05"]', "[1700000000]", '["2024-01-02T03:04:00+05:45"]', '["2024-01-02T00:00:00.000001Z"]',
    ],
    "l_time": [
        '["03:04:05Z"]', '["03:04:05+02:00"]', '["03:04:05"]', '["03:04"]', "[3661]", '["03:04:05.5-05:30"]',
        '["00:00"]', '["03:00:00.000001"]',
    ],
    "l_uuid": ['["{12345678-1234-5678-1234-567812345678}"]'],
}


def normalize(value):
    """DataPoint._normalize_identity_value."""
    if isinstance(value, str):
        return value.lower().replace(" ", "_").replace("'", "")
    return str(value)


def stored(value):
    """The JSON form cognee's storage encoder (cognee.modules.storage.utils.JSONEncoder) writes."""
    if isinstance(value, enum.Enum):
        return stored(value.value)
    if isinstance(value, pydantic.RootModel):
        # A scalar `$defs` entry; storage holds its root, as `model_dump` does.
        return stored(value.root)
    if isinstance(value, (datetime.datetime, datetime.date, datetime.time)):
        return value.isoformat()
    if isinstance(value, UUID):
        return str(value)
    if isinstance(value, list):
        return [stored(item) for item in value]
    return value


def unrepresentable(value):
    """Why a validated value cannot be held by a serde_json `Value`, if so."""
    if isinstance(value, enum.Enum):
        return unrepresentable(value.value)
    if isinstance(value, pydantic.RootModel):
        return unrepresentable(value.root)
    if isinstance(value, float) and not math.isfinite(value):
        return "non-finite float"
    if isinstance(value, int) and not isinstance(value, bool) and not (-(2**63) <= value < 2**64):
        return "integer outside the i64/u64 range"
    if isinstance(value, list):
        return next((r for r in map(unrepresentable, value) if r), None)
    return None


# JSON `null` is left out for a field that is not itself nullable. A property
# missing from `required` becomes `X | None = None` in the generated model, so
# Python takes `null` there; Rust treats the schema as written and refuses it.
# That is a separate, pre-existing difference from the scalar coercions here.
for _field, _inputs in CASES.items():
    if _field != "opt_num":
        _inputs[:] = [raw for raw in _inputs if raw != "null"]


def main():
    schema = {
        "title": "Thing",
        "type": "object",
        "properties": FIELDS,
        "$defs": DEFS,
    }
    model = graph_schema_to_graph_model(schema)
    rows = []
    for field, inputs in CASES.items():
        for raw in inputs:
            row = {"field": field, "input": raw}
            try:
                instance = model.model_validate_json('{"%s": %s}' % (field, raw))
            except pydantic.ValidationError:
                row["accepted"] = False
                rows.append(row)
                continue
            value = getattr(instance, field)
            row["accepted"] = True
            row["str"] = str(value)
            row["id"] = str(uuid5(NAMESPACE_OID, "Thing:" + normalize(value)))
            gap = unrepresentable(value)
            if gap:
                row["gap"] = gap
            else:
                row["stored"] = stored(value)
            rows.append(row)
    json.dump({"defs": DEFS, "fields": FIELDS, "pydantic": pydantic.VERSION, "rows": rows}, sys.stdout, indent=1)
    print()


if __name__ == "__main__":
    main()
