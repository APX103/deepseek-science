# 学科扩展插件体系

> **本文回答**：跨学科的特殊数据处理与可视化（化学/生物/地学/材料/天文…）怎么接入而不污染核心？需要做哪些调研？

> 状态：调研中（用户明确指出「需要做一些深入的调研和研究」）。本文是**问题框架 + 调研清单**，不是定案。

---

## 问题陈述

用户原话：「在不同学科领域的一些比较特殊的数据处理和图像展示之类的东西，都需要做一些深入的调研和研究。」

科研工作台不能只服务 CS/写作场景。不同学科有专用的：
- **数据格式**：化学（.pdb/.mol/.sdf/.cif）、地学（.nc/.hdf/GeoTIFF）、天文（.fits）、材料（.cif/.POSCAR）、生信（.bam/.vcf/.fastq）。
- **处理工具链**：化学（RDKit）、生信（Biopython）、地学（GDAL/xarray）、天文（astropy）。
- **可视化**：3D 分子、地理地图、天体图像、晶体结构、基因组浏览器。

**核心矛盾**：这些能力①体量大（不可能全进核心）、②依赖重（多需 Python 原生库甚至 C 扩展）、③长尾（每个学科用户群小）、④演进快。

**结论**：必须**插件化**，核心只提供「扩展点」和「通用骨架」，学科能力按需挂载。

---

## 已有的基础扩展点

- 前端已规划分子可视化组件（3Dmol.js，支持 .pdb/.mol/.sdf/.xyz/.cif/.mmtf/.gro 等）——作为学科可视化的起点。
- 前端 `ArtifactPreview` 按扩展名派发预览（tex/pdf/md/img/csv/json/mol/code）。
- `python` 工具可跑任意 Python 库（沙箱内）。
- skill 是 markdown 指令 + 可选 kernel——天然适合承载学科「操作指南」。

**启示**：学科扩展 = **前端预览组件 + 后端处理工具/数据格式 + skill 指南** 三件套，需统一打包分发。

---

## 插件体系设计（草案）

### 扩展点清单

一个「学科插件」可在以下点扩展核心：

| 扩展点 | 作用 | 机制 |
|--------|------|------|
| **Tools** | 注册学科专用工具（如 `parse_mol`、`render_fits`） | 实现 `Tool` trait，插件清单声明 |
| **Skills** | 提供学科操作 skill（如「蛋白质结构分析」） | SKILL.md 文件，放入插件目录 |
| **File viewers** | 前端对新格式的预览 | 前端组件注册（扩展名→组件） |
| **Data parsers** | 后端解析专用格式为通用中间态 | Rust trait 或 Python 模块（沙箱内） |
| **Templates** | 学科专用 LaTeX 模板（如 ACS/Beamer） | template 目录 |
| **Sandbox presets** | 预装学科 Python 环境（RDKit/Biopython） | venv requirements 预设 |

### 插件清单格式（草案）

```toml
# plugins/chemistry/plugin.toml
[plugin]
name = "chemistry"
version = "0.1.0"
description = "化学结构处理与可视化"
domain = "chemistry"

[tools]
# Rust 编译的插件：声明动态库入口
# 或 Python 实现：声明沙箱内可调用的模块
backend = "rust"   # | "python" | "skill-only"
module = "dss_plugin_chemistry"

[viewers]
".pdb" = "MoleculeView"
".sdf" = "MoleculeView"
".mol" = "MoleculeView"

[skills]
"chem-structure" = "skills/chem-structure/SKILL.md"

[sandbox_preset]
requirements = ["rdkit", "openbabel"]

[templates]
"acs" = "templates/acs/"
```

### 分发形态

> 待定（调研）

- **A. 内置可选**：随包附带常用学科插件（编译期 feature flag），用户在设置里开关。
- **B. 独立包**：插件是独立 crate / pip 包，运行时发现并加载（Rust 动态加载 `libloading`，或 Python 插件经沙箱）。
- **C. 混合**：核心内置轻量通用（分子/CSV/图），重度学科（生信/天文）走独立包。

**倾向 C**。Rust 动态加载插件（dlopen .so/.dylib）有 ABI 稳定性风险，初期以「编译期 feature + Python 沙箱插件」为主。

---

## 学科调研清单

> 以下需逐项调研，产出落到 [research/](../research/)。每项调研：数据格式、主流工具链、Python 生态、可视化需求、是否已有 Rust 实现。

### 化学 / 材料
- **格式**：.pdb .mol .sdf .cif .POSCAR .xyz .smi .inchi
- **工具**：RDKit（Python，C++ 底）、Open Babel、ASE（Atomic Simulation Environment）、pymatgen。
- **可视化**：3D 结构（3Dmol.js 前端已规划）、晶体结构、电子密度。
- **Rust 生态**：调研 `chemfiles`（Rust 绑定？）、是否有纯 Rust 的 SDF/PDB 解析。
- **典型任务**：结构优化、性质预测、反应映射、材料筛选。

### 生物 / 生信
- **格式**：.fastq .bam .vcf .bed .gtf .fasta .pdb（蛋白）
- **工具**：Biopython、pysam、scanpy（单细胞）、DESeq2（R）。
- **可视化**：基因组浏览器、热图、t-SNE/UMAP、系统发生树。
- **Rust 生态**：`needletail`（FASTA/Q）、`noodles`（BAM/CRAM/VCF，纯 Rust，质量高）。
- **沙箱**：生信重度依赖 R（DESeq2/Seurat）——R 语言支持的需求来源。
- **典型任务**：序列分析、变异 calling、差异表达、单细胞聚类。

### 地学 / GIS / 气候
- **格式**：.nc（NetCDF）.hdf GeoTIFF .shp
- **工具**：GDAL、xarray、rasterio、cartopy、QGIS。
- **可视化**：地图（投影）、 raster、等值线、时间序列动画。
- **Rust 生态**：`netcdf` crate、`gdal` 绑定、`geo`/`geo-types`。
- **典型任务**：气候数据分析、遥感处理、空间统计。

### 天文
- **格式**：.fits（标准）
- **工具**：astropy、CCDProc、specutils。
- **可视化**：FITS 图像（色映射、缩放）、光谱、天球图。
- **Rust 生态**：`fitsrs`（纯 Rust FITS 解析，调研成熟度）。
- **典型任务**：测光、光谱分析、交叉证认。

### 物理 / 工程
- **格式**：HDF5、自定义二进制、MC 模拟输出（ROOT）。
- **工具**：NumPy/SciPy、h5py、pytables、uproot（ROOT）。
- **可视化**：直方图（含误差）、n-1 图、相空间。
- **典型任务**：模拟分析、拟合、统计推断。

### 通用数据科学
- **格式**：CSV/Parquet/Arrow/Excel。
- **工具**：pandas、polars、duckdb。
- **可视化**：交互图（plotly/bokeh）、统计图。
- **Rust 生态**：`polars`（纯 Rust，强）、`arrow`、`duckdb` 绑定——**这块 Rust 生态强，可原生集成**。

---

## 调研优先级建议

> 待与用户确认

建议先调研**用户最可能用到的 2-3 个学科**深入，其余先留扩展点骨架。可考虑的切入点：
- 若主力用户偏化学/材料 → RDKit + 3Dmol 深化。
- 若偏生信 → noodles + R 支持 + 基因组视图。
- 若偏数据科学 → polars 原生集成 + plotly。

> 决策：**插件体系骨架在路线图后期搭建，具体学科按需调研驱动**。不预先实现所有学科，避免空转。

---

## 对核心架构的预留要求

- ✅ `Tool` trait 可注册第三方实现。
- ✅ skill 体系支持插件目录加载。
- ✅ 前端 `ArtifactPreview` 的扩展名→组件映射可扩展（可能需小改支持外部注册）。
- ✅ 沙箱 venv 预设（`install_packages` 已有，扩展为 preset）。
- 待定：插件分发形态（编译期 feature vs 动态加载 vs Python 包）。
- 待定：是否引入 `libloading` 动态插件（ABI 风险）。

---

下一步：读 [08 递进式开发路线图](roadmap.md)。
