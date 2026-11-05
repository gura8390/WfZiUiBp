# 上游来源与许可

- 文字识别的功能和接口以 Umi-OCR Paddle 2.1.5 为参考。Umi-OCR 版权归 hiroi-sora（2023），采用 MIT。项目附带 [Umi-OCR 许可原文](LICENSE-UMI-OCR.txt)。[Umi-OCR 2.1.5](https://github.com/hiroi-sora/Umi-OCR/releases/tag/v2.1.5)
- 应用、窗口和托盘图标沿用 Umi-OCR 2.1.5 的 `umiocr.ico`，保留原图的 16、32、48 和 256 像素资源。SHA-256 为 `5efa713be8e51cd1367d032f676e195697a99ffd4e5e6711617a77490adc9703`。[上游图标文件](https://github.com/hiroi-sora/Umi-OCR/blob/6563ff0ee9152c16b321e12e950de77bf627514d/UmiOCR-data/qt_res/images/icons/umiocr.ico)
- 工具栏透明位图由同一版本的原版 SVG 图标栅格化生成，运行时按主题着色。[上游图标目录](https://github.com/hiroi-sora/Umi-OCR/tree/6563ff0ee9152c16b321e12e950de77bf627514d/UmiOCR-data/qt_res/images/icons)
- 旧版检测、方向分类、识别模型与字典来自本机 Umi-OCR 发行版中的 PaddleOCR-json 插件。检测和识别权重转换为 ONNX，字典按原字节保留。各 Paddle 原件及 ONNX 转换产物的 SHA-256 可见 [模型来源清单](models/model-provenance.json)，适用的 [Apache 2.0 许可](models/PaddleOCR-LICENSE.txt) 随模型附带。[PaddleOCR-json](https://github.com/hiroi-sora/PaddleOCR-json) · [PaddleOCR](https://github.com/PaddlePaddle/PaddleOCR)
- PP-OCRv6 Tiny 使用 PaddlePaddle 发布的 ONNX 检测和识别权重。识别字典提取自官方识别模型的 `inference.yml`，固定修订、文件地址和 SHA-256 见 [v6 来源清单](models/v6-provenance.json)。许可为 Apache 2.0。[检测模型](https://huggingface.co/PaddlePaddle/PP-OCRv6_tiny_det_onnx) · [识别模型](https://huggingface.co/PaddlePaddle/PP-OCRv6_tiny_rec_onnx/tree/2612ab37152ae0a677521bae4e1e3d4fb4cf7c30)
- 随附的 Windows x64 ONNX Runtime 1.24.2 文件取自官方 wheel。对应校验值、[MIT 许可](models/ONNX-Runtime-LICENSE.txt) 和 [第三方声明](models/ONNX-Runtime-ThirdPartyNotices.txt) 与运行库一起保留。[运行库来源清单](models/runtime-provenance.json) · [ONNX Runtime 1.24.2](https://pypi.org/project/onnxruntime/1.24.2/)
- [Rust 依赖许可清单](THIRD-PARTY-RUST.txt) 记录 Windows x64 发行版的 299 个锁定依赖，并附上从 crate 归档和固定上游修订收集的许可及归属声明。清单标记了剩余 16 项 crate 级许可原文缺项，其中 `simd_helpers 0.1.0` 只有 SPDX MIT 声明。[Cargo.lock](Cargo.lock)
- 安装版和便携版附带 Microsoft Visual C++ v143 的四个 x64 运行库 DLL，来源为构建工具的 Redistributable 目录。其使用及再分发受 Microsoft 软件许可条款约束。[Microsoft 运行库分发说明](https://learn.microsoft.com/en-us/cpp/windows/redistributing-visual-cpp-files)
- Windows 安装包使用 NSIS 3.12 生成，安装包压缩使用 zlib。[NSIS 许可](https://nsis.sourceforge.io/Docs/AppendixI.html)
