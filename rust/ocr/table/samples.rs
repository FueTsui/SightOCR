//! Real images plus frozen OCR boxes; expected structure was checked separately.
//! Set SIGHTOCR_TABLE_SAMPLE_DUMP=1 to inspect TSVs without changing the fixtures.
use super::{group_text_rows, infer_columns, reconstruct, straighten, Rect, TextLine};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, path::Path};

#[derive(Deserialize)]
struct Fixtures {
    samples: Vec<Sample>,
}

#[derive(Deserialize)]
struct Sample {
    id: String,
    image: String,
    image_sha256: String,
    input_scale: u32,
    coordinates: String,
    lines: Vec<FrozenLine>,
}

#[derive(Deserialize)]
struct FrozenLine {
    text: String,
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
    /// Original rotated OCR box; present for tilted photos so the frozen test
    /// also covers straightening.
    quad: Option<[[f32; 2]; 4]>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Expected {
    size: [u32; 2],
    columns: Option<usize>,
    atomic_columns: Option<usize>,
    logical_rows: usize,
    header_rows: Option<usize>,
    data: Option<Vec<Vec<String>>>,
    /// Single-cell title rows above and note rows below a framed table.
    outside_rows: Option<[usize; 2]>,
}

fn compact(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

fn layout(id: &str) -> (Vec<Vec<String>>, Expected) {
    let fixtures: Fixtures = serde_json::from_str(include_str!(
        "../../../tests/fixtures/table-lines/fixtures.json"
    ))
    .unwrap();
    let mut expected: BTreeMap<String, Expected> = serde_json::from_str(include_str!(
        "../../../tests/fixtures/table-lines/sample-expectations.json"
    ))
    .unwrap();
    let sample = fixtures
        .samples
        .into_iter()
        .find(|sample| sample.id == id)
        .unwrap();
    let expected = expected.remove(id).unwrap();
    assert_eq!(sample.input_scale, 2);
    assert_eq!(sample.coordinates, "original_image");
    let source = std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join(&sample.image)).unwrap();
    assert_eq!(
        format!("{:x}", Sha256::digest(&source)),
        sample.image_sha256,
        "{id} original image changed"
    );
    let mut image = image::load_from_memory(&source).unwrap().into_rgba8();
    assert_eq!(
        [image.width(), image.height()],
        expected.size,
        "{id} image changed"
    );
    let quads: Option<Vec<_>> = sample
        .lines
        .iter()
        .map(|line| line.quad.map(|quad| quad.map(|[x, y]| (x, y))))
        .collect();
    let mut lines: Vec<_> = sample
        .lines
        .into_iter()
        .map(|line| {
            assert!(
                line.left.is_finite()
                    && line.top.is_finite()
                    && line.right.is_finite()
                    && line.bottom.is_finite()
            );
            assert!(line.left <= line.right && line.top <= line.bottom);
            TextLine {
                text: line.text,
                rect: Rect {
                    left: line.left,
                    top: line.top,
                    right: line.right,
                    bottom: line.bottom,
                },
            }
        })
        .collect();
    if let Some((level, rects)) = quads.and_then(|quads| straighten(&image, &quads)) {
        for (line, rect) in lines.iter_mut().zip(rects) {
            line.rect = rect;
        }
        image = level;
    }
    let tsv = reconstruct(&lines, &image);
    if std::env::var_os("SIGHTOCR_TABLE_SAMPLE_DUMP").is_some() {
        let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/table-samples-layout");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join(format!("{id}.tsv")), &tsv).unwrap();
    }
    let grid: Vec<Vec<String>> = tsv
        .lines()
        .map(|row| row.split('\t').map(str::to_owned).collect())
        .collect();
    let columns = expected.columns.or(expected.atomic_columns).unwrap();
    assert_eq!(
        grid.len(),
        expected.logical_rows,
        "{id}: wrong logical row count\n{tsv}"
    );
    let [above, below] = expected.outside_rows.unwrap_or([0, 0]);
    for (index, row) in grid.iter().enumerate() {
        let outside = index < above || index >= grid.len() - below;
        assert_eq!(
            row.len(),
            if outside { 1 } else { columns },
            "{id}: row {} has wrong atomic column count\n{tsv}",
            index + 1
        );
    }
    (grid, expected)
}

fn cell(grid: &[Vec<String>], row: usize, column: usize, expected: &str) {
    assert_eq!(
        compact(&grid[row - 1][column - 1]),
        compact(expected),
        "wrong cell at row {row}, column {column}"
    );
}

fn data_matrix(grid: &[Vec<String>], expected: &Expected) {
    let header_rows = expected.header_rows.unwrap();
    let data = expected.data.as_ref().unwrap();
    assert_eq!(grid.len() - header_rows, data.len());
    for (row, values) in data.iter().enumerate() {
        for (column, value) in values.iter().enumerate() {
            cell(grid, header_rows + row + 1, column + 1, value);
        }
    }
}

#[test]
fn table3_original_and_upscaled_text_find_nine_columns_without_vertical_rules() {
    #[derive(Deserialize)]
    struct BoxSamples {
        samples: Vec<BoxSample>,
    }
    #[derive(Deserialize)]
    struct BoxSample {
        id: String,
        lines: Vec<FrozenLine>,
    }

    for (variant, json) in [
        (
            "original",
            include_str!("../../../tests/fixtures/table-lines/original-boxes.json"),
        ),
        (
            "Rust Lanczos3 2x",
            include_str!("../../../tests/fixtures/table-lines/fixtures.json"),
        ),
    ] {
        let fixtures: BoxSamples = serde_json::from_str(json).unwrap();
        let lines: Vec<_> = fixtures
            .samples
            .into_iter()
            .find(|sample| sample.id == "table3")
            .unwrap()
            .lines
            .into_iter()
            .map(|line| TextLine {
                text: line.text,
                rect: Rect {
                    left: line.left,
                    top: line.top,
                    right: line.right,
                    bottom: line.bottom,
                },
            })
            .collect();
        let mut ordered: Vec<_> = lines
            .iter()
            .filter(|line| !line.text.trim().is_empty())
            .collect();
        ordered.sort_by(|a, b| {
            a.rect
                .center_y()
                .total_cmp(&b.rect.center_y())
                .then(a.rect.left.total_cmp(&b.rect.left))
        });
        let mut heights: Vec<_> = ordered.iter().map(|line| line.rect.height()).collect();
        heights.sort_by(f32::total_cmp);
        let height = heights[heights.len() / 2];
        let rows = group_text_rows(&ordered, height);
        let columns = infer_columns(&rows, height, &[]);
        assert_eq!(columns.len(), 8,
            "{variant}: text geometry alone must find eight interior cuts / nine columns: {columns:?}");
    }
}

#[test]
fn table1_contract_form_preserves_multiline_cells_and_signature_rows() {
    let (grid, _) = layout("table1");
    cell(&grid, 1, 2, "中国建筑 合同管理表格");
    cell(&grid, 1, 3, "");
    cell(&grid, 2, 2, "合同评审表");
    cell(&grid, 2, 3, "表格编号");
    cell(&grid, 3, 2, "");
    cell(&grid, 3, 3, "CCSTC-CM-B0118");
    cell(&grid, 4, 1, "主办单位");
    cell(&grid, 5, 3, "合同价(元):370,000.00");
    cell(&grid, 7, 1, "合同简要内容");
    for part in ["甲方:", "乙方:", "项目名称:", "一、工作内容:"] {
        assert!(
            grid[6][1].contains(part),
            "contract body split into separate cells: {part}"
        );
    }
    cell(&grid, 7, 3, "");
    cell(&grid, 10, 1, "项目负责人");
    cell(&grid, 10, 2, "同意 签字:马翔");
    cell(&grid, 11, 1, "市场商务负责人");
    cell(&grid, 11, 2, "签字:卢永强");
    cell(&grid, 15, 1, "绿发中心总经理");
    cell(&grid, 15, 2, "签字:齐贺");
}

#[test]
fn table2_legal_categories_keep_seven_atomic_columns_and_group_anchors() {
    let (grid, _) = layout("table2");
    for (column, header) in [
        (1, "类型"),
        (2, "数量"),
        (3, "现行"),
        (4, "尚未生效"),
        (6, "公布日期"),
        (7, "施行日期"),
    ] {
        cell(&grid, 1, column, header);
    }
    cell(&grid, 1, 5, "");
    cell(&grid, 2, 1, "宪法");
    cell(&grid, 3, 1, "法律");
    cell(&grid, 3, 2, "314");
    cell(&grid, 3, 3, "308");
    for row in 4..=8 {
        for column in 1..=3 {
            cell(&grid, row, column, "");
        }
    }
    cell(&grid, 9, 1, "行政法规");
    cell(&grid, 9, 2, "613");
    cell(&grid, 9, 3, "608");
    for row in 10..=13 {
        for column in 1..=3 {
            cell(&grid, row, column, "");
        }
    }
    cell(&grid, 20, 1, "附件");
    cell(&grid, 20, 2, "4");
    cell(&grid, 24, 1, "总计");
    cell(&grid, 24, 2, "18920");
    for column in 3..=7 {
        cell(&grid, 24, column, "");
    }
}

#[test]
fn table3_partial_vertical_rules_preserve_all_sixteen_by_nine_data_cells() {
    let (grid, expected) = layout("table3");
    data_matrix(&grid, &expected);
    cell(&grid, 1, 1, "部份大型屋苑成交呎價");
    cell(&grid, 2, 3, "成交宗數");
    cell(&grid, 2, 6, "呎價(元)");
    cell(&grid, 4, 1, "港島區");
    cell(&grid, 9, 1, "九龍區");
    cell(&grid, 14, 1, "新界區");
    cell(&grid, 12, 3, "3");
}

#[test]
fn table4_missing_horizontal_rules_preserve_all_eighteen_by_five_data_cells() {
    let (grid, expected) = layout("table4");
    data_matrix(&grid, &expected);
    cell(&grid, 1, 1, "科别");
    cell(&grid, 2, 1, "");
    cell(&grid, 1, 2, "门诊人次");
    cell(&grid, 1, 3, "");
    cell(&grid, 1, 4, "构成");
    cell(&grid, 1, 5, "");
    for column in [2, 4] {
        cell(&grid, 2, column, "2017年");
    }
    for column in [3, 5] {
        cell(&grid, 2, column, "2018年");
    }
}

#[test]
fn table5_long_descriptions_remain_in_three_row_merged_cells() {
    let (grid, _) = layout("table5");
    cell(&grid, 1, 1, "建筑类型");
    cell(&grid, 1, 2, "地下空间开发利用指标");
    cell(&grid, 1, 3, "");
    cell(&grid, 1, 4, "得分");
    cell(&grid, 2, 1, "住宅建筑");
    cell(
        &grid,
        2,
        2,
        "地下建筑面积与地上建筑面积的比率Rr 地下一层建筑面积与总用地面积 的比率Rp",
    );
    cell(&grid, 5, 1, "公共建筑");
    cell(
        &grid,
        5,
        2,
        "地下建筑面积与总用地面积之比Rp1 地下一层建筑面积与总用地面积 的比率Rp",
    );
    for row in [3, 4, 6, 7] {
        cell(&grid, row, 1, "");
        cell(&grid, row, 2, "");
    }
    for (index, score) in ["5", "7", "12", "5", "7", "12"].iter().enumerate() {
        cell(&grid, index + 2, 4, score);
    }
    cell(&grid, 2, 3, "5%<R,<20%");
    cell(&grid, 4, 3, "Rr≥35%且Rp<60%");
    cell(&grid, 7, 3, "Rp1≥1.0且Rp<60%");
}

#[test]
fn table6_vertical_labels_and_multiline_standards_anchor_in_the_first_merged_row() {
    let (grid, _) = layout("table6");
    cell(&grid, 1, 1, "机组类型");
    cell(&grid, 1, 2, "");
    cell(&grid, 1, 3, "能效指标");
    cell(&grid, 1, 4, "参照标准");
    cell(&grid, 1, 5, "评分要求");
    cell(&grid, 1, 6, "");
    cell(&grid, 2, 1, "多联式空调 (热泵)机组");
    cell(&grid, 2, 3, "制冷综合性能 系数 [IPLV(C)]");
    cell(
        &grid,
        2,
        4,
        "现行国家标 准《公共建 筑节能设 计标准》 GB 50189",
    );
    cell(&grid, 3, 1, "锅 炉");
    cell(&grid, 4, 1, "");
    cell(&grid, 3, 2, "燃煤");
    cell(&grid, 4, 2, "燃油燃气");
    cell(&grid, 3, 5, "提高3个 百分点");
    cell(&grid, 4, 6, "提高4个 百分点");
    cell(&grid, 5, 4, "现行有关 国家标准");
    cell(&grid, 5, 5, "节能评价值");
    cell(&grid, 5, 6, "1级能效 等级限值");
    for row in [6, 7] {
        for column in 4..=6 {
            cell(&grid, row, column, "");
        }
    }
    cell(&grid, 6, 3, "热效率值(n)");
    cell(&grid, 8, 1, "得分");
    for column in 2..=4 {
        cell(&grid, 8, column, "");
    }
    cell(&grid, 8, 5, "5分");
    cell(&grid, 8, 6, "10分");
}

#[test]
fn table7_grouped_headers_preserve_five_threshold_columns_and_scores() {
    let (grid, _) = layout("table7");
    cell(&grid, 1, 1, "建筑气候 区划");
    cell(&grid, 2, 1, "");
    cell(&grid, 1, 2, "人均住宅用地指标A(m2)");
    for column in 3..=6 {
        cell(&grid, 1, column, "");
    }
    cell(&grid, 1, 7, "得分");
    cell(&grid, 2, 7, "");
    for (index, header) in [
        "平均3层 及以下",
        "平均4~ 6层",
        "平均7~ 9层",
        "平均10~ 18层",
        "平均19层 及以上",
    ]
    .iter()
    .enumerate()
    {
        cell(&grid, 2, index + 2, header);
    }
    // These are frozen OCR glyphs, not replacements for the image's Roman numerals.
    cell(&grid, 3, 1, "I、Ⅵ");
    cell(&grid, 4, 1, "");
    cell(&grid, 5, 1, "Ⅲ、Ⅳ 、V");
    cell(&grid, 6, 1, "");
    for (index, score) in ["15", "20", "15", "20"].iter().enumerate() {
        cell(&grid, index + 3, 7, score);
    }
    for (index, limit) in ["33<A≤36", "27<A≤30", "20<A≤21", "16<A≤17", "12<A≤13"]
        .iter()
        .enumerate()
    {
        cell(&grid, 3, index + 2, limit);
    }
    for (index, limit) in ["A≤33", "A≤24", "A≤19", "A≤15", "A≤11"].iter().enumerate() {
        cell(&grid, 6, index + 2, limit);
    }
}

#[test]
fn table8_titles_and_notes_stay_outside_a_diagonal_headed_timetable() {
    let (grid, _) = layout("table8");
    cell(&grid, 1, 1, "上海市光明初级中学");
    cell(&grid, 2, 1, "2025学年度第一学期");
    cell(&grid, 3, 1, "初一(1)班课程表");
    cell(&grid, 4, 1, "星期/科目/时间");
    cell(&grid, 4, 2, "");
    for (index, day) in ["星期一", "星期二", "星期三", "星期四", "星期五"]
        .iter()
        .enumerate()
    {
        cell(&grid, 4, index + 3, day);
    }
    cell(&grid, 5, 1, "第1节");
    cell(&grid, 5, 2, "08:00-08:40");
    cell(&grid, 5, 7, "数学");
    // The full-width break rows keep their single merged label.
    cell(&grid, 7, 1, "大课间");
    cell(&grid, 7, 3, "大课间活动");
    cell(&grid, 10, 3, "午间休息");
    for row in [7, 10] {
        for column in 4..=7 {
            cell(&grid, row, column, "");
        }
    }
    cell(&grid, 12, 5, "人工智能基础");
    cell(&grid, 16, 1, "注:眼保健操时间:10:50-10:55,14:40-14:45。");
    cell(
        &grid,
        20,
        1,
        "六至八年级综合实践活动课程由学校统筹实施,保障每周1课时。",
    );
}

#[test]
fn table9_keystoned_photo_keeps_side_columns_and_notes() {
    let (grid, _) = layout("table9");
    cell(&grid, 1, 1, "苏州市景范中学作息时间表");
    cell(&grid, 2, 1, "(2020-2021 学年第一学期)");
    // The heading spans the merged 上午/下午 column and the time column.
    cell(&grid, 3, 1, "时间");
    cell(&grid, 3, 2, "");
    cell(&grid, 3, 3, "内 容");
    cell(&grid, 4, 1, "上午");
    cell(&grid, 4, 2, "7: 45");
    cell(&grid, 4, 3, "大预备铃");
    for row in 5..=11 {
        cell(&grid, row, 1, "");
    }
    cell(&grid, 6, 3, "升旗仪式(周一) 大课间(周二~周五)");
    // A blank merged cell of its own between the morning and afternoon groups.
    cell(&grid, 12, 1, "");
    cell(&grid, 12, 3, "午餐、午检、午休");
    cell(&grid, 13, 1, "下午");
    cell(&grid, 19, 2, "17: 30");
    cell(&grid, 20, 1, "备注");
    cell(
        &grid,
        20,
        2,
        "1、每周一上午大课间时间为升旗仪式。2、教职工在校工作时间按校规定执行。3、本表自2020年9月1日起执行。",
    );
}
