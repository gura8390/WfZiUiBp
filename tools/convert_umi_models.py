"""Build model assets from the Windows Umi-OCR Paddle v2.1.5 package."""

import argparse
import ctypes as ffi
import hashlib
import importlib.metadata
import importlib.util
import json
import os
from pathlib import Path
import shutil
import tempfile

from google.protobuf import descriptor_pb2, descriptor_pool, message_factory


MODELS = {
    "det": "ch_PP-OCRv3_det_infer",
    "cls": "ch_ppocr_mobile_v2.0_cls_infer",
    "rec_chinese": "ch_PP-OCRv3_rec_infer",
    "rec_chinese_cht": "chinese_cht_mobile_v2.0_rec_infer",
    "rec_en": "en_PP-OCRv3_rec_infer",
    "rec_japan": "japan_PP-OCRv3_rec_infer",
    "rec_korean": "korean_PP-OCRv3_rec_infer",
    "rec_cyrillic": "cyrillic_PP-OCRv3_rec_infer",
}


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def modern_converter():
    distribution = importlib.metadata.distribution("paddle2onnx")
    if distribution.version != "1.3.1":
        raise RuntimeError("Model conversion requires paddle2onnx==1.3.1")
    extension = next(
        entry for entry in distribution.files
        if entry.name.startswith("paddle2onnx_cpp2py_export") and entry.suffix == ".pyd"
    )
    spec = importlib.util.spec_from_file_location(
        "paddle2onnx_cpp2py_export", distribution.locate_file(extension),
    )
    converter = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(converter)
    return converter


def dynamic_recurrent_width(data):
    # These are the field numbers of the public ONNX protobuf. A partial
    # descriptor preserves the unparsed operators, initializers and attributes.
    schema = descriptor_pb2.FileDescriptorProto(name="ocr_shape.proto", syntax="proto2")
    messages = {
        "Dim": [("dim_value", 1, 3), ("dim_param", 2, 9)],
        "Shape": [("dim", 1, 11, "Dim", True)],
        "Tensor": [("shape", 2, 11, "Shape")],
        "Type": [("tensor", 1, 11, "Tensor")],
        "Info": [("type", 2, 11, "Type")],
        "Graph": [("input", 11, 11, "Info", True), ("output", 12, 11, "Info", True)],
        "Model": [("graph", 7, 11, "Graph")],
    }
    for name, fields in messages.items():
        message = schema.message_type.add(name=name)
        for field_name, number, kind, *target in fields:
            field = message.field.add(
                name=field_name, number=number, type=kind, label=3 if len(target) > 1 else 1,
            )
            if target:
                field.type_name = "." + target[0]
    pool = descriptor_pool.DescriptorPool()
    pool.Add(schema)
    model_class = message_factory.GetMessageClass(pool.FindMessageTypeByName("Model"))
    model = model_class.FromString(data)
    input_width = model.graph.input[0].type.tensor.shape.dim[3]
    sequence = model.graph.output[0].type.tensor.shape.dim[1]
    for dim, parameter in [(input_width, "width"), (sequence, "sequence")]:
        dim.ClearField("dim_value")
        dim.dim_param = parameter
    return model.SerializeToString()


def convert_recurrent_model(converter, staged, model, weights):
    # The bundled converter predates the RNN mapper required by Traditional
    # Chinese. The standalone extension from the 1.3.1 wheel handles it.
    (staged / "input.pdmodel").write_bytes(model)
    (staged / "input.pdiparams").write_bytes(weights)
    original_cwd = Path.cwd()
    try:
        os.chdir(staged)
        data = converter.export(
            "input.pdmodel", "input.pdiparams", 11, True, False, True, True,
            True, {}, "onnxruntime", "", "", False,
        )
        return dynamic_recurrent_width(data)
    finally:
        os.chdir(original_cwd)
        (staged / "input.pdmodel").unlink()
        (staged / "input.pdiparams").unlink()


def reshape_classifier(converter, staged, data):
    # Use the same classifier size as Umi's C++ runner. The bundled weights were
    # exported with an illustrative 32×100 input, although the graph supports more.
    (staged / "input.onnx").write_bytes(data)
    original_cwd = Path.cwd()
    try:
        os.chdir(staged)
        converter.optimize("input.onnx", "output.onnx", {"x": [-1, 3, 48, 192]})
        return Path("output.onnx").read_bytes()
    finally:
        os.chdir(original_cwd)
        (staged / "input.onnx").unlink(missing_ok=True)
        (staged / "output.onnx").unlink(missing_ok=True)


def convert(plugin, output):
    converter_path = plugin / "paddle2onnx.dll"
    modern = modern_converter()
    with os.add_dll_directory(str(plugin)):
        converter = ffi.CDLL(str(converter_path))
        # The exact ABI used by the converter bundled in this release. The memory
        # overload also works when the Umi installation path contains Chinese.
        export = getattr(
            converter,
            "?Export@paddle2onnx@@YA_NPEBXH0HPEAPEADPEAHH_N3333PEAUCustomOp@1@HPEBD@Z",
        )
        export.argtypes = (
            ffi.c_void_p, ffi.c_int, ffi.c_void_p, ffi.c_int,
            ffi.POINTER(ffi.c_void_p), ffi.POINTER(ffi.c_int), ffi.c_int,
            ffi.c_bool, ffi.c_bool, ffi.c_bool, ffi.c_bool, ffi.c_bool,
            ffi.c_void_p, ffi.c_int, ffi.c_char_p,
        )
        export.restype = ffi.c_bool
        records = {}
        source_models = plugin / "models"
        output.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix=".converting-", dir=output) as temporary:
            staged = Path(temporary)
            for target, source in MODELS.items():
                model = (source_models / source / "inference.pdmodel").read_bytes()
                weights = (source_models / source / "inference.pdiparams").read_bytes()
                if target == "rec_chinese_cht":
                    exported = convert_recurrent_model(modern, staged, model, weights)
                    converter_version = "Paddle2ONNX wheel 1.3.1, input width made dynamic"
                else:
                    result = ffi.c_void_p()
                    length = ffi.c_int()
                    if not export(
                        model, len(model), weights, len(weights),
                        ffi.byref(result), ffi.byref(length), 11,
                        True, False, True, False, True, None, 0, b"onnxruntime",
                    ) or not result.value or length.value <= 0:
                        raise RuntimeError(f"ONNX export failed for {source}")
                    # The short-lived build process releases the native converter's
                    # output buffers on exit, without mixing C runtimes.
                    exported = ffi.string_at(result, length.value)
                    converter_version = "paddle2onnx.dll bundled in Umi-OCR"
                    if target == "cls":
                        exported = reshape_classifier(modern, staged, exported)
                        converter_version += ", 48×192 input shape via Paddle2ONNX wheel 1.3.1"
                if not exported:
                    raise RuntimeError(f"ONNX export failed for {source}")
                name = f"{target}.onnx"
                (staged / name).write_bytes(exported)
                records[name] = {
                    "source": source,
                    "converter": converter_version,
                    "paddle_model_sha256": sha256(model),
                    "paddle_weights_sha256": sha256(weights),
                    "sha256": sha256(exported),
                }
                if target.startswith("rec_"):
                    dictionary = "dict_" + target.removeprefix("rec_") + ".txt"
                    shutil.copyfile(source_models / dictionary, staged / dictionary)
                    records[dictionary] = {"sha256": sha256((staged / dictionary).read_bytes())}
                print(f"{source} -> {name} ({len(exported):,} bytes)", flush=True)
            manifest = {
                "source_release": "https://github.com/hiroi-sora/Umi-OCR/releases/tag/v2.1.5",
                "converter_sha256": sha256(converter_path.read_bytes()),
                "requested_opset": 11,
                "automatic_opset_upgrade": True,
                "assets": records,
            }
            (staged / "model-provenance.json").write_text(
                json.dumps(manifest, indent=2) + "\n", encoding="utf-8",
            )
            for file in staged.iterdir():
                file.replace(output / file.name)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("umi_installation", type=Path)
    parser.add_argument("--output", type=Path, default=Path(__file__).resolve().parents[1] / "models")
    options = parser.parse_args()
    plugin = options.umi_installation.resolve() / "UmiOCR-data" / "plugins" / "win7_x64_PaddleOCR-json"
    convert(plugin, options.output.resolve())


if __name__ == "__main__":
    main()
