//! 定义布局加载端口共享的不可变模型和程序注册项。

pub(in crate::application) mod preview;
pub(in crate::application) mod upgrade;

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use suzushiro_text_format::is_canonical_sha256;
use thiserror::Error;

/// 当前程序能够读取的布局配置 schema。
pub const LAYOUT_SCHEMA_VERSION: u32 = 1;

/// 工作表或字段在输出工作簿中的生成方式。
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LayoutGenerationMode {
    /// 生成并正常显示。
    Visible,
    /// 生成完整内容但在工作簿中隐藏。
    Hidden,
    /// 完全不生成对应表或字段。
    Omitted,
}

/// 输出单元格使用的稳定值格式。
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LayoutValueFormat {
    /// 保留字符串语义，不进行数值转换。
    Text,
    /// 使用无小数位的整数显示。
    Integer,
    /// 使用允许小数位的数值显示。
    Decimal,
    /// 使用百分比显示。
    Percentage,
    /// 使用日期时间显示。
    DateTime,
    /// 使用 JSON 文本显示复杂原始结构。
    Json,
}

/// 用户在数据工作簿中可以使用的编辑方式。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LayoutEditor {
    /// 数据只用于查看，用户不能修改。
    ReadOnly,
    /// 用户在是与否之间选择。
    Boolean,
    /// 用户从注册枚举分类中选择。
    Enumeration,
    /// 用户输入整数。
    Integer,
    /// 用户输入普通文本。
    Text,
}

impl LayoutGenerationMode {
    /// 返回布局表和工作簿元数据使用的稳定文本值。
    pub(crate) const fn stable_value(self) -> &'static str {
        match self {
            Self::Visible => "visible",
            Self::Hidden => "hidden",
            Self::Omitted => "omitted",
        }
    }
}

impl LayoutValueFormat {
    /// 返回布局表和工作簿元数据使用的稳定文本值。
    pub(crate) const fn stable_value(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Integer => "integer",
            Self::Decimal => "decimal",
            Self::Percentage => "percentage",
            Self::DateTime => "date_time",
            Self::Json => "json",
        }
    }
}

impl LayoutEditor {
    /// 返回布局表和工作簿元数据使用的稳定文本值。
    pub(crate) const fn stable_value(self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::Boolean => "boolean",
            Self::Enumeration => "enumeration",
            Self::Integer => "integer",
            Self::Text => "text",
        }
    }
}

/// 样式使用的水平对齐方式。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LayoutHorizontalAlignment {
    /// 内容靠左对齐。
    Left,
    /// 内容水平居中。
    Center,
    /// 内容靠右对齐。
    Right,
}

/// 样式使用的垂直对齐方式。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LayoutVerticalAlignment {
    /// 内容靠上对齐。
    Top,
    /// 内容垂直居中。
    Center,
    /// 内容靠下对齐。
    Bottom,
}

/// 以百分之一 Excel 列宽保存可复算的布局宽度。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct LayoutColumnWidth(u16);

impl LayoutColumnWidth {
    const MIN_HUNDREDTHS: u16 = 1;
    const MAX_HUNDREDTHS: u16 = 25_500;

    /// 建立 Excel 支持范围内且精确到百分之一的列宽。
    pub fn from_hundredths(value: u16) -> Result<Self, LayoutModelError> {
        if (Self::MIN_HUNDREDTHS..=Self::MAX_HUNDREDTHS).contains(&value) {
            Ok(Self(value))
        } else {
            Err(LayoutModelError::ColumnWidthOutOfRange { value })
        }
    }

    /// 返回用于稳定摘要的百分之一列宽。
    pub const fn hundredths(self) -> u16 {
        self.0
    }
}

/// 一个已经通过注册表核对的输出工作表布局。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WorkbookSheetLayout {
    stable_key: String,
    generation: LayoutGenerationMode,
    display_name: String,
    order: u32,
    freeze_cell: Option<String>,
    default_filter: bool,
    description: String,
    required: bool,
}

impl WorkbookSheetLayout {
    /// 由布局适配器建立一个已经通过注册表核对的工作表配置。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        stable_key: String,
        generation: LayoutGenerationMode,
        display_name: String,
        order: u32,
        freeze_cell: Option<String>,
        default_filter: bool,
        description: String,
        required: bool,
    ) -> Self {
        Self {
            stable_key,
            generation,
            display_name,
            order,
            freeze_cell,
            default_filter,
            description,
            required,
        }
    }

    /// 返回程序识别工作表时使用的稳定键。
    pub fn stable_key(&self) -> &str {
        &self.stable_key
    }

    /// 返回工作表的生成和显示方式。
    pub const fn generation(&self) -> LayoutGenerationMode {
        self.generation
    }

    /// 返回最终 Excel 标签名称。
    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    /// 返回工作表在最终工作簿中的唯一顺序值。
    pub const fn order(&self) -> u32 {
        self.order
    }

    /// 返回可选的 A1 冻结位置。
    pub fn freeze_cell(&self) -> Option<&str> {
        self.freeze_cell.as_deref()
    }

    /// 返回是否默认启用表头筛选。
    pub const fn default_filter(&self) -> bool {
        self.default_filter
    }

    /// 返回工作表数据口径说明。
    pub fn description(&self) -> &str {
        &self.description
    }

    /// 返回工作表是否属于执行所需的必需结构。
    pub const fn required(&self) -> bool {
        self.required
    }
}

/// 一个已经通过模型路径和编辑器注册表核对的输出字段布局。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WorkbookFieldLayout {
    sheet_key: String,
    stable_key: String,
    generation: LayoutGenerationMode,
    display_name: String,
    order: u32,
    width: LayoutColumnWidth,
    value_format: LayoutValueFormat,
    wrap: bool,
    description: String,
    model_path: String,
    editor: LayoutEditor,
    enum_category: Option<String>,
    required: bool,
}

impl WorkbookFieldLayout {
    /// 由布局适配器建立一个已经通过注册表核对的字段配置。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        sheet_key: String,
        stable_key: String,
        generation: LayoutGenerationMode,
        display_name: String,
        order: u32,
        width: LayoutColumnWidth,
        value_format: LayoutValueFormat,
        wrap: bool,
        description: String,
        model_path: String,
        editor: LayoutEditor,
        enum_category: Option<String>,
        required: bool,
    ) -> Self {
        Self {
            sheet_key,
            stable_key,
            generation,
            display_name,
            order,
            width,
            value_format,
            wrap,
            description,
            model_path,
            editor,
            enum_category,
            required,
        }
    }

    /// 返回字段所属工作表的稳定键。
    pub fn sheet_key(&self) -> &str {
        &self.sheet_key
    }

    /// 返回程序和公式引用字段时使用的稳定键。
    pub fn stable_key(&self) -> &str {
        &self.stable_key
    }

    /// 返回字段的生成和显示方式。
    pub const fn generation(&self) -> LayoutGenerationMode {
        self.generation
    }

    /// 返回最终列标题。
    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    /// 返回字段在所属工作表中的唯一顺序值。
    pub const fn order(&self) -> u32 {
        self.order
    }

    /// 返回精确到百分之一的 Excel 列宽。
    pub const fn width(&self) -> LayoutColumnWidth {
        self.width
    }

    /// 返回输出单元格使用的值格式。
    pub const fn value_format(&self) -> LayoutValueFormat {
        self.value_format
    }

    /// 返回单元格是否自动换行。
    pub const fn wrap(&self) -> bool {
        self.wrap
    }

    /// 返回字段含义和数据口径说明。
    pub fn description(&self) -> &str {
        &self.description
    }

    /// 返回经过注册表核对的来源模型路径。
    pub fn model_path(&self) -> &str {
        &self.model_path
    }

    /// 返回字段允许使用的编辑器。
    pub const fn editor(&self) -> LayoutEditor {
        self.editor
    }

    /// 返回字段绑定的稳定枚举分类；只读输出字段也可以声明枚举值域。
    pub fn enum_category(&self) -> Option<&str> {
        self.enum_category.as_deref()
    }

    /// 返回字段是否属于执行所需的必需结构。
    pub const fn required(&self) -> bool {
        self.required
    }
}

/// 一个枚举稳定值及其可编辑中文标签。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WorkbookLayoutEnumOption {
    category_key: String,
    stable_value: String,
    label: String,
    order: u32,
    description: String,
}

impl WorkbookLayoutEnumOption {
    /// 由布局适配器建立一个已经通过注册表核对的枚举选项。
    pub(crate) fn new(
        category_key: String,
        stable_value: String,
        label: String,
        order: u32,
        description: String,
    ) -> Self {
        Self {
            category_key,
            stable_value,
            label,
            order,
            description,
        }
    }

    /// 返回枚举分类稳定键。
    pub fn category_key(&self) -> &str {
        &self.category_key
    }

    /// 返回程序保存和比较的枚举稳定值。
    pub fn stable_value(&self) -> &str {
        &self.stable_value
    }

    /// 返回最终下拉框显示的中文标签。
    pub fn label(&self) -> &str {
        &self.label
    }

    /// 返回选项在所属枚举分类中的唯一顺序值。
    pub const fn order(&self) -> u32 {
        self.order
    }

    /// 返回枚举选项的语义说明。
    pub fn description(&self) -> &str {
        &self.description
    }
}

/// 一个稳定样式键及其用户可调整外观。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WorkbookLayoutStyle {
    stable_key: String,
    background_color: String,
    font_color: String,
    bold: bool,
    horizontal_alignment: LayoutHorizontalAlignment,
    vertical_alignment: LayoutVerticalAlignment,
    wrap: bool,
    description: String,
}

impl WorkbookLayoutStyle {
    /// 由布局适配器建立一个已经通过注册表核对的样式配置。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        stable_key: String,
        background_color: String,
        font_color: String,
        bold: bool,
        horizontal_alignment: LayoutHorizontalAlignment,
        vertical_alignment: LayoutVerticalAlignment,
        wrap: bool,
        description: String,
    ) -> Self {
        Self {
            stable_key,
            background_color,
            font_color,
            bold,
            horizontal_alignment,
            vertical_alignment,
            wrap,
            description,
        }
    }

    /// 返回程序引用样式时使用的稳定键。
    pub fn stable_key(&self) -> &str {
        &self.stable_key
    }

    /// 返回规范化为大写的六位 RGB 背景色。
    pub fn background_color(&self) -> &str {
        &self.background_color
    }

    /// 返回规范化为大写的六位 RGB 字体色。
    pub fn font_color(&self) -> &str {
        &self.font_color
    }

    /// 返回字体是否加粗。
    pub const fn bold(&self) -> bool {
        self.bold
    }

    /// 返回水平对齐方式。
    pub const fn horizontal_alignment(&self) -> LayoutHorizontalAlignment {
        self.horizontal_alignment
    }

    /// 返回垂直对齐方式。
    pub const fn vertical_alignment(&self) -> LayoutVerticalAlignment {
        self.vertical_alignment
    }

    /// 返回使用该样式的单元格是否自动换行。
    pub const fn wrap(&self) -> bool {
        self.wrap
    }

    /// 返回样式用途说明。
    pub fn description(&self) -> &str {
        &self.description
    }
}

/// 已完成结构、注册项和唯一性校验的不可变布局。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkbookLayout {
    schema_version: u32,
    template_name: String,
    purpose: String,
    sheets: Vec<WorkbookSheetLayout>,
    fields: Vec<WorkbookFieldLayout>,
    enum_options: Vec<WorkbookLayoutEnumOption>,
    styles: Vec<WorkbookLayoutStyle>,
    content_sha256: String,
}

impl WorkbookLayout {
    /// 仅省略可选获取方式列，保留用户布局并重新计算实际生成布局摘要。
    pub(crate) fn without_ship_acquisition(&self) -> Result<Self, serde_json::Error> {
        let mut layout = self.clone();
        for field in &mut layout.fields {
            if field.sheet_key == "loadout_plan" && field.stable_key == "acquisition" {
                field.generation = LayoutGenerationMode::Omitted;
            }
        }
        layout.content_sha256 = layout_content_sha256(
            layout.schema_version,
            &layout.template_name,
            &layout.purpose,
            &layout.sheets,
            &layout.fields,
            &layout.enum_options,
            &layout.styles,
        )?;
        Ok(layout)
    }
    /// 隐藏字段同样属于生成请求；原始数据表要求保留完整展示证据。
    pub(crate) fn read_scope(&self) -> crate::domain::GameReadScope {
        let dependencies = crate::application::WorkbookProjectionV4::field_read_dependencies();
        let active_sheets: std::collections::BTreeSet<&str> = self
            .sheets()
            .iter()
            .filter(|sheet| sheet.generation() != LayoutGenerationMode::Omitted)
            .map(|sheet| sheet.stable_key())
            .collect();
        let mut skill_effects = false;
        let mut technology = false;
        let mut weapons = false;
        let mut equipment_skills = false;
        for field in self.fields() {
            if field.generation() == LayoutGenerationMode::Omitted
                || !active_sheets.contains(field.sheet_key())
            {
                continue;
            }
            if field.sheet_key() == "raw_data" {
                skill_effects = true;
                technology = true;
                weapons = true;
                equipment_skills = true;
                break;
            }
            let Some(dependency) =
                dependencies.get(&(field.sheet_key().to_owned(), field.stable_key().to_owned()))
            else {
                continue;
            };
            match dependency {
                FieldReadDependency::ShipSkillEffects => skill_effects = true,
                FieldReadDependency::ShipTechnology => technology = true,
                FieldReadDependency::EquipmentWeapons => weapons = true,
                FieldReadDependency::EquipmentSkillEffects => equipment_skills = true,
            }
        }
        crate::domain::GameReadScope::with_ship_skill_effects(skill_effects)
            .with_ship_technology(technology)
            .with_equipment_details(weapons, equipment_skills)
    }

    /// 汇总全部已校验配置并确认 schema 与内容摘要格式。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        schema_version: u32,
        template_name: String,
        purpose: String,
        sheets: Vec<WorkbookSheetLayout>,
        fields: Vec<WorkbookFieldLayout>,
        enum_options: Vec<WorkbookLayoutEnumOption>,
        styles: Vec<WorkbookLayoutStyle>,
        content_sha256: String,
    ) -> Result<Self, LayoutModelError> {
        if schema_version != LAYOUT_SCHEMA_VERSION {
            return Err(LayoutModelError::SchemaVersionUnsupported {
                expected: LAYOUT_SCHEMA_VERSION,
                actual: schema_version,
            });
        }
        if !is_canonical_sha256(&content_sha256) {
            return Err(LayoutModelError::InvalidContentDigest);
        }
        Ok(Self {
            schema_version,
            template_name,
            purpose,
            sheets,
            fields,
            enum_options,
            styles,
            content_sha256,
        })
    }

    /// 返回布局文件使用的 schema 版本。
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// 返回布局模板显示名称。
    pub fn template_name(&self) -> &str {
        &self.template_name
    }

    /// 返回布局用途说明。
    pub fn purpose(&self) -> &str {
        &self.purpose
    }

    /// 返回按最终顺序规范化的工作表配置。
    pub fn sheets(&self) -> &[WorkbookSheetLayout] {
        &self.sheets
    }

    /// 返回按工作表和最终列顺序规范化的字段配置。
    pub fn fields(&self) -> &[WorkbookFieldLayout] {
        &self.fields
    }

    /// 返回一张输出表按布局顺序保留的可生成字段。
    ///
    /// 科技派生表使用模板字段，隐藏字段保留，省略字段不进入生成列。
    pub(crate) fn generated_fields_for_sheet(&self, sheet_key: &str) -> Vec<&WorkbookFieldLayout> {
        let template_key = super::technology_template_key(sheet_key);
        self.fields
            .iter()
            .filter(|field| {
                field.sheet_key() == template_key
                    && field.generation() != LayoutGenerationMode::Omitted
            })
            .collect()
    }

    /// 返回按分类和显示顺序规范化的枚举选项。
    pub fn enum_options(&self) -> &[WorkbookLayoutEnumOption] {
        &self.enum_options
    }

    /// 返回按稳定键规范化的样式配置。
    pub fn styles(&self) -> &[WorkbookLayoutStyle] {
        &self.styles
    }

    /// 返回基于规范布局模型计算的小写 SHA-256。
    pub fn content_sha256(&self) -> &str {
        &self.content_sha256
    }
}

/// 布局加载与运行设置叠加共用同一规范摘要输入。
pub(crate) fn layout_content_sha256(
    schema_version: u32,
    template_name: &str,
    purpose: &str,
    sheets: &[WorkbookSheetLayout],
    fields: &[WorkbookFieldLayout],
    enum_options: &[WorkbookLayoutEnumOption],
    styles: &[WorkbookLayoutStyle],
) -> Result<String, serde_json::Error> {
    #[derive(Serialize)]
    struct LayoutDigest<'a> {
        schema_version: u32,
        template_name: &'a str,
        purpose: &'a str,
        sheets: &'a [WorkbookSheetLayout],
        fields: &'a [WorkbookFieldLayout],
        enum_options: &'a [WorkbookLayoutEnumOption],
        styles: &'a [WorkbookLayoutStyle],
    }
    suzushiro_content_digest::sha256_sorted_json(&LayoutDigest {
        schema_version,
        template_name,
        purpose,
        sheets,
        fields,
        enum_options,
        styles,
    })
}

/// 程序承诺生成的一张稳定工作表。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegisteredLayoutSheet {
    stable_key: String,
    required: bool,
}

impl RegisteredLayoutSheet {
    /// 登记一个稳定工作表及其必需性。
    pub fn new(stable_key: impl Into<String>, required: bool) -> Self {
        Self {
            stable_key: stable_key.into(),
            required,
        }
    }

    /// 返回登记的工作表稳定键。
    pub fn stable_key(&self) -> &str {
        &self.stable_key
    }

    /// 返回该工作表是否禁止省略。
    pub const fn required(&self) -> bool {
        self.required
    }
}

/// 字段触发的游戏读取范围。不进入布局文件，也不进入注册表摘要。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FieldReadDependency {
    ShipSkillEffects,
    ShipTechnology,
    EquipmentWeapons,
    EquipmentSkillEffects,
}

/// 程序承诺生成的一个字段及其不可编辑契约。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegisteredLayoutField {
    sheet_key: String,
    stable_key: String,
    model_path: String,
    allowed_formats: BTreeSet<LayoutValueFormat>,
    editor: LayoutEditor,
    required: bool,
    enum_category: Option<String>,
    read_dependency: Option<FieldReadDependency>,
}

impl RegisteredLayoutField {
    /// 登记一个字段的模型路径、格式、编辑器、必需性和可选枚举分类。
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        sheet_key: impl Into<String>,
        stable_key: impl Into<String>,
        model_path: impl Into<String>,
        allowed_formats: impl IntoIterator<Item = LayoutValueFormat>,
        editor: LayoutEditor,
        required: bool,
        enum_category: Option<String>,
    ) -> Self {
        Self {
            sheet_key: sheet_key.into(),
            stable_key: stable_key.into(),
            model_path: model_path.into(),
            allowed_formats: allowed_formats.into_iter().collect(),
            editor,
            required,
            enum_category,
            read_dependency: None,
        }
    }

    /// 标记该字段会请求的游戏读取范围。
    pub(crate) fn with_read_dependency(mut self, dependency: FieldReadDependency) -> Self {
        self.read_dependency = Some(dependency);
        self
    }

    /// 返回该字段附带的读取依赖；没有时不扩大读取范围。
    pub(crate) const fn read_dependency(&self) -> Option<FieldReadDependency> {
        self.read_dependency
    }

    /// 返回字段所属工作表稳定键。
    pub fn sheet_key(&self) -> &str {
        &self.sheet_key
    }

    /// 返回字段稳定键。
    pub fn stable_key(&self) -> &str {
        &self.stable_key
    }

    /// 返回程序允许的固定模型路径。
    pub fn model_path(&self) -> &str {
        &self.model_path
    }

    /// 返回布局文件可以为字段选择的值格式集合。
    pub fn allowed_formats(&self) -> &BTreeSet<LayoutValueFormat> {
        &self.allowed_formats
    }

    /// 返回字段固定使用的编辑器。
    pub const fn editor(&self) -> LayoutEditor {
        self.editor
    }

    /// 返回该字段是否禁止省略。
    pub const fn required(&self) -> bool {
        self.required
    }

    /// 返回字段绑定的稳定枚举分类；只读输出字段也可以声明枚举值域。
    pub fn enum_category(&self) -> Option<&str> {
        self.enum_category.as_deref()
    }
}

/// 程序识别的枚举分类与稳定值，中文标签和顺序仍由布局文件决定。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegisteredLayoutEnumOption {
    category_key: String,
    stable_value: String,
}

impl RegisteredLayoutEnumOption {
    /// 登记程序识别的一对枚举分类和稳定值。
    pub fn new(category_key: impl Into<String>, stable_value: impl Into<String>) -> Self {
        Self {
            category_key: category_key.into(),
            stable_value: stable_value.into(),
        }
    }

    /// 返回枚举分类稳定键。
    pub fn category_key(&self) -> &str {
        &self.category_key
    }

    /// 返回枚举稳定值。
    pub fn stable_value(&self) -> &str {
        &self.stable_value
    }
}

/// 当前程序用于核对稳定工作表、字段、枚举和样式的完整注册表。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkbookLayoutRegistry {
    sheets: Vec<RegisteredLayoutSheet>,
    fields: Vec<RegisteredLayoutField>,
    enum_options: Vec<RegisteredLayoutEnumOption>,
    style_keys: Vec<String>,
}

impl WorkbookLayoutRegistry {
    /// 建立去重并按稳定键排序的完整布局注册表。
    pub fn new(
        mut sheets: Vec<RegisteredLayoutSheet>,
        mut fields: Vec<RegisteredLayoutField>,
        mut enum_options: Vec<RegisteredLayoutEnumOption>,
        mut style_keys: Vec<String>,
    ) -> Result<Self, LayoutModelError> {
        if sheets.is_empty() {
            return Err(LayoutModelError::RegistryEmpty {
                component: "工作表",
            });
        }
        if fields.is_empty() {
            return Err(LayoutModelError::RegistryEmpty {
                component: "字段"
            });
        }
        if enum_options.is_empty() {
            return Err(LayoutModelError::RegistryEmpty {
                component: "枚举项",
            });
        }
        if style_keys.is_empty() {
            return Err(LayoutModelError::RegistryEmpty {
                component: "样式"
            });
        }

        let mut sheet_keys: BTreeSet<String> = BTreeSet::new();
        for sheet in &sheets {
            validate_stable_key("工作表", sheet.stable_key())?;
            if !sheet_keys.insert(sheet.stable_key.clone()) {
                return Err(LayoutModelError::DuplicateRegistryKey {
                    component: "工作表",
                    key: sheet.stable_key.clone(),
                });
            }
        }

        let mut field_keys: BTreeSet<(String, String)> = BTreeSet::new();
        for field in &fields {
            validate_stable_key("字段所属工作表", field.sheet_key())?;
            validate_stable_key("字段", field.stable_key())?;
            if !sheet_keys.contains(field.sheet_key()) {
                return Err(LayoutModelError::RegistryFieldUsesUnknownSheet {
                    sheet_key: field.sheet_key.clone(),
                    field_key: field.stable_key.clone(),
                });
            }
            let parent_is_required = sheets
                .iter()
                .find(|sheet| sheet.stable_key() == field.sheet_key())
                .is_some_and(RegisteredLayoutSheet::required);
            if field.required() && !parent_is_required {
                return Err(LayoutModelError::RequiredFieldUsesOptionalSheet {
                    sheet_key: field.sheet_key.clone(),
                    field_key: field.stable_key.clone(),
                });
            }
            if !field_keys.insert((field.sheet_key.clone(), field.stable_key.clone())) {
                return Err(LayoutModelError::DuplicateRegistryField {
                    sheet_key: field.sheet_key.clone(),
                    field_key: field.stable_key.clone(),
                });
            }
            validate_model_path(field.model_path())?;
            if field.allowed_formats.is_empty() {
                return Err(LayoutModelError::RegistryFieldHasNoFormat {
                    sheet_key: field.sheet_key.clone(),
                    field_key: field.stable_key.clone(),
                });
            }
            let enum_category_is_valid = match field.editor {
                LayoutEditor::Enumeration => field.enum_category.is_some(),
                LayoutEditor::ReadOnly => true,
                LayoutEditor::Boolean | LayoutEditor::Integer | LayoutEditor::Text => {
                    field.enum_category.is_none()
                }
            };
            if !enum_category_is_valid {
                return Err(LayoutModelError::RegistryFieldEnumMismatch {
                    sheet_key: field.sheet_key.clone(),
                    field_key: field.stable_key.clone(),
                });
            }
        }

        validate_unique_stable_keys("样式", &mut style_keys)?;
        let mut enum_keys: BTreeSet<(String, String)> = BTreeSet::new();
        for option in &enum_options {
            validate_stable_key("枚举分类", option.category_key())?;
            validate_stable_key("枚举值", option.stable_value())?;
            if !enum_keys.insert((option.category_key.clone(), option.stable_value.clone())) {
                return Err(LayoutModelError::DuplicateRegistryEnumOption {
                    category_key: option.category_key.clone(),
                    stable_value: option.stable_value.clone(),
                });
            }
        }
        let enum_category_set: BTreeSet<&str> = enum_options
            .iter()
            .map(|option| option.category_key())
            .collect();
        for field in &fields {
            if let Some(category) = field.enum_category()
                && !enum_category_set.contains(category)
            {
                return Err(LayoutModelError::RegistryFieldUsesUnknownEnum {
                    sheet_key: field.sheet_key.clone(),
                    field_key: field.stable_key.clone(),
                    enum_category: category.to_owned(),
                });
            }
        }

        sheets.sort_by(|left, right| left.stable_key.cmp(&right.stable_key));
        fields.sort_by(|left, right| {
            (&left.sheet_key, &left.stable_key).cmp(&(&right.sheet_key, &right.stable_key))
        });
        enum_options.sort_by(|left, right| {
            (&left.category_key, &left.stable_value)
                .cmp(&(&right.category_key, &right.stable_value))
        });
        Ok(Self {
            sheets,
            fields,
            enum_options,
            style_keys,
        })
    }

    /// 返回按稳定键排序的工作表注册项。
    pub fn sheets(&self) -> &[RegisteredLayoutSheet] {
        &self.sheets
    }

    /// 返回按工作表和字段稳定键排序的字段注册项。
    pub fn fields(&self) -> &[RegisteredLayoutField] {
        &self.fields
    }

    /// 返回按分类和稳定值排序的枚举注册项。
    pub fn enum_options(&self) -> &[RegisteredLayoutEnumOption] {
        &self.enum_options
    }

    /// 返回按稳定键排序的样式注册项。
    pub fn style_keys(&self) -> &[String] {
        &self.style_keys
    }

    /// 使用稳定键二分查找工作表注册项。
    pub fn sheet(&self, key: &str) -> Option<&RegisteredLayoutSheet> {
        self.sheets
            .binary_search_by(|sheet| sheet.stable_key().cmp(key))
            .ok()
            .map(|index| &self.sheets[index])
    }

    /// 使用工作表和字段稳定键二分查找字段注册项。
    pub fn field(&self, sheet_key: &str, field_key: &str) -> Option<&RegisteredLayoutField> {
        self.fields
            .binary_search_by(|field| {
                (field.sheet_key(), field.stable_key()).cmp(&(sheet_key, field_key))
            })
            .ok()
            .map(|index| &self.fields[index])
    }
}

/// 校验一组稳定键并原地排序，供注册表建立过程复用。
fn validate_unique_stable_keys(
    component: &'static str,
    values: &mut [String],
) -> Result<(), LayoutModelError> {
    let mut seen: BTreeMap<String, ()> = BTreeMap::new();
    for value in values.iter() {
        validate_stable_key(component, value)?;
        if seen.insert(value.clone(), ()).is_some() {
            return Err(LayoutModelError::DuplicateRegistryKey {
                component,
                key: value.clone(),
            });
        }
    }
    values.sort();
    Ok(())
}

/// 只接受整体以小写字母开头、后续分段允许固定数字索引的 snake_case 稳定键。
fn validate_stable_key(component: &'static str, value: &str) -> Result<(), LayoutModelError> {
    let mut segments = value.split('_');
    let valid_named_segment = |segment: &str| {
        let mut bytes = segment.bytes();
        matches!(bytes.next(), Some(b'a'..=b'z'))
            && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    };
    let valid_numeric_segment = |segment: &str| {
        !segment.is_empty()
            && (segment == "0" || !segment.starts_with('0'))
            && segment.bytes().all(|byte| byte.is_ascii_digit())
    };
    let valid_first = segments.next().is_some_and(valid_named_segment);
    let valid = valid_first
        && segments.all(|segment| valid_named_segment(segment) || valid_numeric_segment(segment));
    if value.is_empty() || !valid {
        Err(LayoutModelError::InvalidStableKey {
            component,
            value: value.to_owned(),
        })
    } else {
        Ok(())
    }
}

/// 校验点分模型路径，不允许表达式或通配符。
fn validate_model_path(value: &str) -> Result<(), LayoutModelError> {
    let valid: bool = value.trim() == value && value.split('.').all(validate_model_path_segment);
    if valid {
        Ok(())
    } else {
        Err(LayoutModelError::InvalidModelPath {
            value: value.to_owned(),
        })
    }
}

/// 接受普通字段、集合 `[]` 或固定数字索引 `[N]` 三种路径片段。
fn validate_model_path_segment(segment: &str) -> bool {
    let (name, index_is_valid) = match segment.split_once('[') {
        Some((name, suffix)) => match suffix.strip_suffix(']') {
            Some(index) => (
                name,
                !index.contains(['[', ']']) && index.bytes().all(|byte| byte.is_ascii_digit()),
            ),
            None => (name, false),
        },
        None => (segment, !segment.contains(']')),
    };
    !name.is_empty()
        && index_is_valid
        && !name.contains(['[', ']'])
        && name.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_alphabetic() || byte == b'_' || (index > 0 && byte.is_ascii_digit())
        })
}

/// 程序布局注册表或不可变布局违反内部模型约束。
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum LayoutModelError {
    /// 注册表没有提供必需的组件集合。
    #[error("布局注册表缺少 {component}")]
    RegistryEmpty { component: &'static str },
    /// 稳定键不符合受限 snake_case 语法。
    #[error("{component} 稳定键 {value:?} 不是小写 snake_case")]
    InvalidStableKey {
        component: &'static str,
        value: String,
    },
    /// 同一类注册项出现重复稳定键。
    #[error("布局注册表重复包含 {component} 稳定键 {key}")]
    DuplicateRegistryKey {
        component: &'static str,
        key: String,
    },
    /// 同一工作表内出现重复字段稳定键。
    #[error("布局注册表重复包含字段 {sheet_key}.{field_key}")]
    DuplicateRegistryField {
        sheet_key: String,
        field_key: String,
    },
    /// 同一枚举分类内出现重复稳定值。
    #[error("布局注册表重复包含枚举项 {category_key}.{stable_value}")]
    DuplicateRegistryEnumOption {
        category_key: String,
        stable_value: String,
    },
    /// 字段引用了不存在的工作表注册项。
    #[error("字段 {sheet_key}.{field_key} 引用了未注册工作表")]
    RegistryFieldUsesUnknownSheet {
        sheet_key: String,
        field_key: String,
    },
    /// 执行必需字段挂在允许省略的工作表下，导致字段约束可以被父表绕过。
    #[error("必需字段 {sheet_key}.{field_key} 不能属于非必需工作表")]
    RequiredFieldUsesOptionalSheet {
        sheet_key: String,
        field_key: String,
    },
    /// 模型路径包含不支持的片段或表达式。
    #[error("模型路径 {value:?} 无效")]
    InvalidModelPath { value: String },
    /// 字段没有声明任何允许的输出格式。
    #[error("字段 {sheet_key}.{field_key} 没有允许的值格式")]
    RegistryFieldHasNoFormat {
        sheet_key: String,
        field_key: String,
    },
    /// 字段编辑器与枚举分类组合不合法。
    #[error("字段 {sheet_key}.{field_key} 的编辑器和枚举分类不一致")]
    RegistryFieldEnumMismatch {
        sheet_key: String,
        field_key: String,
    },
    /// 枚举字段引用了不存在的枚举分类。
    #[error("字段 {sheet_key}.{field_key} 引用了未注册枚举分类 {enum_category}")]
    RegistryFieldUsesUnknownEnum {
        sheet_key: String,
        field_key: String,
        enum_category: String,
    },
    /// 精确列宽超出 Excel 支持范围。
    #[error("列宽百分之一值 {value} 超出 1 至 25500")]
    ColumnWidthOutOfRange { value: u16 },
    /// 不可变布局使用了程序不支持的 schema 版本。
    #[error("布局 schema 不支持: expected={expected}, actual={actual}")]
    SchemaVersionUnsupported { expected: u32, actual: u32 },
    /// 规范布局摘要不是预期的小写 SHA-256。
    #[error("布局内容摘要必须是 64 位小写十六进制")]
    InvalidContentDigest,
}

#[cfg(test)]
mod tests {
    use super::{
        LayoutEditor, LayoutModelError, LayoutValueFormat, RegisteredLayoutEnumOption,
        RegisteredLayoutField, RegisteredLayoutSheet, WorkbookLayoutRegistry,
    };

    #[test]
    fn stable_layout_values_match_serialized_contract() {
        use super::{LayoutEditor, LayoutGenerationMode, LayoutValueFormat};
        for value in [
            LayoutGenerationMode::Visible,
            LayoutGenerationMode::Hidden,
            LayoutGenerationMode::Omitted,
        ] {
            assert_eq!(serde_json::to_value(value).unwrap(), value.stable_value());
        }
        for value in [
            LayoutValueFormat::Text,
            LayoutValueFormat::Integer,
            LayoutValueFormat::Decimal,
            LayoutValueFormat::Percentage,
            LayoutValueFormat::DateTime,
            LayoutValueFormat::Json,
        ] {
            assert_eq!(serde_json::to_value(value).unwrap(), value.stable_value());
        }
        for value in [
            LayoutEditor::ReadOnly,
            LayoutEditor::Boolean,
            LayoutEditor::Enumeration,
            LayoutEditor::Integer,
            LayoutEditor::Text,
        ] {
            assert_eq!(serde_json::to_value(value).unwrap(), value.stable_value());
        }
    }

    #[test]
    fn generated_fields_keep_order_and_map_technology_sheets() {
        use super::{
            LayoutColumnWidth, LayoutEditor, LayoutGenerationMode, LayoutValueFormat,
            WorkbookFieldLayout, WorkbookLayout,
        };

        let field = |sheet: &str, key: &str, generation: LayoutGenerationMode| {
            WorkbookFieldLayout::new(
                sheet.to_owned(),
                key.to_owned(),
                generation,
                key.to_owned(),
                1,
                LayoutColumnWidth::from_hundredths(1_000).unwrap(),
                LayoutValueFormat::Text,
                false,
                String::new(),
                "GameState.note".to_owned(),
                LayoutEditor::ReadOnly,
                None,
                false,
            )
        };
        let layout = WorkbookLayout::new(
            1,
            "样本".to_owned(),
            "字段筛选".to_owned(),
            Vec::new(),
            vec![
                field("loadout_plan", "visible", LayoutGenerationMode::Visible),
                field("loadout_plan", "hidden", LayoutGenerationMode::Hidden),
                field("loadout_plan", "omitted", LayoutGenerationMode::Omitted),
                field("ship_technology", "bonus", LayoutGenerationMode::Visible),
                field("other", "kept", LayoutGenerationMode::Visible),
            ],
            Vec::new(),
            Vec::new(),
            "a".repeat(64),
        )
        .unwrap();

        assert_eq!(
            layout
                .generated_fields_for_sheet("loadout_plan")
                .iter()
                .map(|field| field.stable_key())
                .collect::<Vec<_>>(),
            ["visible", "hidden"]
        );
        assert_eq!(
            layout
                .generated_fields_for_sheet("ship_technology:hull")
                .iter()
                .map(|field| field.stable_key())
                .collect::<Vec<_>>(),
            ["bonus"]
        );
    }

    #[test]
    fn registered_fields_carry_read_dependencies_outside_the_registry_digest() {
        use super::FieldReadDependency;
        use crate::application::WorkbookProjectionV4;

        let registry = WorkbookProjectionV4::layout_registry().unwrap();
        assert_eq!(
            registry
                .field("loadout_plan", "static_summary")
                .unwrap()
                .read_dependency(),
            Some(FieldReadDependency::ShipSkillEffects)
        );
        assert_eq!(
            registry
                .field("loadout_plan", "technology_level")
                .unwrap()
                .read_dependency(),
            Some(FieldReadDependency::ShipTechnology)
        );
        assert_eq!(
            registry
                .field("ship_technology", "technology_level")
                .unwrap()
                .read_dependency(),
            Some(FieldReadDependency::ShipTechnology)
        );
        assert_eq!(
            registry
                .field("equipment_inventory", "weapons_json")
                .unwrap()
                .read_dependency(),
            Some(FieldReadDependency::EquipmentWeapons)
        );
        assert_eq!(
            registry
                .field("equipment_inventory", "effect_summary")
                .unwrap()
                .read_dependency(),
            Some(FieldReadDependency::EquipmentSkillEffects)
        );
        assert!(
            registry
                .field("loadout_plan", "name")
                .unwrap()
                .read_dependency()
                .is_none()
        );
    }

    #[test]
    fn registry_accepts_collection_and_fixed_numeric_model_indexes() {
        let registry = registry_with_model_path("GameState.ships[].slots[1].equipment.name")
            .expect("集合和固定数字索引应属于受限模型路径语法");

        assert_eq!(
            registry.fields()[0].model_path(),
            "GameState.ships[].slots[1].equipment.name"
        );
    }

    #[test]
    fn registry_accepts_fixed_numeric_segments_in_stable_field_keys() {
        let registry = WorkbookLayoutRegistry::new(
            vec![RegisteredLayoutSheet::new("ships", false)],
            vec![RegisteredLayoutField::new(
                "ships",
                "slot_1_equipment_name",
                "GameState.ships[].slots[1].equipment.name",
                [LayoutValueFormat::Text],
                LayoutEditor::ReadOnly,
                false,
                None,
            )],
            vec![RegisteredLayoutEnumOption::new(
                "generation_mode",
                "visible",
            )],
            vec!["read_only".to_owned()],
        )
        .expect("固定槽位编号应属于稳定字段键语法");

        assert!(registry.field("ships", "slot_1_equipment_name").is_some());
    }

    #[test]
    fn registry_accepts_a_read_only_field_with_a_stable_enum_domain() {
        let registry = WorkbookLayoutRegistry::new(
            vec![RegisteredLayoutSheet::new("execution_results", true)],
            vec![RegisteredLayoutField::new(
                "execution_results",
                "status",
                "WorkbookProjectionV4.execution_results[].status",
                [LayoutValueFormat::Text],
                LayoutEditor::ReadOnly,
                true,
                Some("execution_status".to_owned()),
            )],
            vec![RegisteredLayoutEnumOption::new(
                "execution_status",
                "success",
            )],
            vec!["read_only".to_owned()],
        )
        .expect("只读输出字段应允许绑定稳定枚举值域");

        assert_eq!(
            registry
                .field("execution_results", "status")
                .unwrap()
                .enum_category(),
            Some("execution_status")
        );
    }

    #[test]
    fn registry_rejects_noncanonical_numeric_segments_in_stable_field_keys() {
        for stable_key in [
            "slot_1foo_equipment_name",
            "slot_01_equipment_name",
            "slot__1_equipment_name",
        ] {
            let error = WorkbookLayoutRegistry::new(
                vec![RegisteredLayoutSheet::new("ships", false)],
                vec![RegisteredLayoutField::new(
                    "ships",
                    stable_key,
                    "GameState.ships[].slots[1].equipment.name",
                    [LayoutValueFormat::Text],
                    LayoutEditor::ReadOnly,
                    false,
                    None,
                )],
                vec![RegisteredLayoutEnumOption::new(
                    "generation_mode",
                    "visible",
                )],
                vec!["read_only".to_owned()],
            )
            .unwrap_err();

            assert_eq!(
                error,
                LayoutModelError::InvalidStableKey {
                    component: "字段",
                    value: stable_key.to_owned(),
                }
            );
        }
    }

    #[test]
    fn registry_rejects_wildcard_model_indexes() {
        let error = registry_with_model_path("GameState.ships[*].slots[1]").unwrap_err();

        assert_eq!(
            error,
            LayoutModelError::InvalidModelPath {
                value: "GameState.ships[*].slots[1]".to_owned(),
            }
        );
    }

    #[test]
    fn registry_rejects_required_field_on_optional_sheet() {
        let error = WorkbookLayoutRegistry::new(
            vec![RegisteredLayoutSheet::new("ships", false)],
            vec![RegisteredLayoutField::new(
                "ships",
                "instance_id",
                "GameState.ships[].instance_id",
                [LayoutValueFormat::Text],
                LayoutEditor::ReadOnly,
                true,
                None,
            )],
            vec![RegisteredLayoutEnumOption::new(
                "generation_mode",
                "visible",
            )],
            vec!["read_only".to_owned()],
        )
        .unwrap_err();

        assert_eq!(
            error,
            LayoutModelError::RequiredFieldUsesOptionalSheet {
                sheet_key: "ships".to_owned(),
                field_key: "instance_id".to_owned(),
            }
        );
    }

    /// 建立只包含一个字段的最小完整注册表。
    fn registry_with_model_path(
        model_path: &str,
    ) -> Result<WorkbookLayoutRegistry, LayoutModelError> {
        WorkbookLayoutRegistry::new(
            vec![RegisteredLayoutSheet::new("ships", false)],
            vec![RegisteredLayoutField::new(
                "ships",
                "slot_equipment_name",
                model_path,
                [LayoutValueFormat::Text],
                LayoutEditor::ReadOnly,
                false,
                None,
            )],
            vec![RegisteredLayoutEnumOption::new(
                "generation_mode",
                "visible",
            )],
            vec!["read_only".to_owned()],
        )
    }
}
