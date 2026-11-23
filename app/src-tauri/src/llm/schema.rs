//! 动态 JSON Schema：按提取列生成（Instructor 方法论 ① Schema-first）。
//! 每列字段顺序固定为 reasoning → quote → value（生成顺序即思考顺序）。

use serde_json::{json, Value};

use crate::llm::ColumnSpec;

/// 完整版（B）：reasoning + quote + value 三字段，供引用校验。
pub fn build_extraction_schema(cols: &[ColumnSpec]) -> Value {
    build_schema(cols, false)
}

/// A/B 对比用直出版（A）：仅 value 字段，无 reasoning/quote、无引用校验。
pub fn build_direct_schema(cols: &[ColumnSpec]) -> Value {
    build_schema(cols, true)
}

fn build_schema(cols: &[ColumnSpec], value_only: bool) -> Value {
    // preserve_order feature：Map 保持插入序，properties 按 column_index 升序
    let mut properties = serde_json::Map::new();
    for c in cols {
        let field = if value_only {
            json!({
                "type": "object",
                "description": c.describe(),
                "properties": {
                    "value": {
                        "type": "string",
                        "description": c.value_hint()
                    }
                },
                "required": ["value"],
                "additionalProperties": false
            })
        } else {
            json!({
                "type": "object",
                "description": c.describe(),
                "properties": {
                    "reasoning": {
                        "type": "string",
                        "description": "定位依据：简述在原文何处找到该信息"
                    },
                    "quote": {
                        "type": "string",
                        "description": "原文逐字引用（不得改写）"
                    },
                    "value": {
                        "type": "string",
                        "description": c.value_hint()
                    }
                },
                "required": ["reasoning", "quote", "value"],
                "additionalProperties": false
            })
        };
        properties.insert(format!("col_{}", c.column_index), field);
    }
    let required: Vec<String> = cols
        .iter()
        .map(|c| format!("col_{}", c.column_index))
        .collect();
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cols() -> Vec<ColumnSpec> {
        vec![
            ColumnSpec {
                column_index: 2,
                keyword: "合同编号".into(),
                ..Default::default()
            },
            ColumnSpec {
                column_index: 4,
                keyword: "监控账号".into(),
                ..Default::default()
            },
            ColumnSpec {
                column_index: 3,
                keyword: "甲方".into(),
                ..Default::default()
            },
        ]
    }

    #[test]
    fn schema_structure_and_field_order() {
        let mut cols = cols();
        cols.sort_by_key(|c| c.column_index); // 调用方负责升序
        let s = build_extraction_schema(&cols);
        assert_eq!(s["type"], "object");
        assert_eq!(s["additionalProperties"], false);
        let props = s["properties"].as_object().unwrap();
        // properties 按 column_index 升序（preserve_order）
        let keys: Vec<&String> = props.keys().collect();
        assert_eq!(keys, ["col_2", "col_3", "col_4"]);
        // 列 description = 用户关键词原文
        assert_eq!(props["col_3"]["description"], "甲方");
        // 每列三字段且顺序 reasoning → quote → value
        let col3 = props["col_3"]["properties"].as_object().unwrap();
        let field_keys: Vec<&String> = col3.keys().collect();
        assert_eq!(field_keys, ["reasoning", "quote", "value"]);
        assert_eq!(
            props["col_3"]["required"],
            json!(["reasoning", "quote", "value"])
        );
        assert_eq!(props["col_3"]["additionalProperties"], false);
        // required 覆盖全部列
        assert_eq!(s["required"], json!(["col_2", "col_3", "col_4"]));
        // 序列化后顺序仍保持
        let text = serde_json::to_string(&s).unwrap();
        let i2 = text.find("col_2").unwrap();
        let i3 = text.find("col_3").unwrap();
        let i4 = text.find("col_4").unwrap();
        assert!(i2 < i3 && i3 < i4, "{text}");
    }

    #[test]
    fn direct_schema_has_only_value() {
        let s = build_direct_schema(&cols());
        let col2 = &s["properties"]["col_2"];
        let keys: Vec<&String> = col2["properties"].as_object().unwrap().keys().collect();
        assert_eq!(keys, ["value"]);
        assert_eq!(col2["required"], json!(["value"]));
        assert_eq!(col2["description"], "合同编号");
    }

    #[test]
    fn schema_description_and_value_hint_follow_extras() {
        let cols = vec![
            ColumnSpec {
                column_index: 2,
                keyword: "金额".into(),
                definition: "指甲乙双方约定的含税总金额".into(),
                info_type: "number".into(),
            },
            ColumnSpec {
                column_index: 3,
                keyword: "签订日期".into(),
                info_type: "date".into(),
                ..Default::default()
            },
        ];
        let s = build_extraction_schema(&cols);
        let col2 = &s["properties"]["col_2"];
        assert_eq!(
            col2["description"],
            "金额（信息类型：数字；定义：指甲乙双方约定的含税总金额）"
        );
        assert_eq!(
            col2["properties"]["value"]["description"],
            "规整后的提取值（数字，保留原文千分位格式）；找不到则为空字符串"
        );
        let col3 = &s["properties"]["col_3"];
        assert_eq!(col3["description"], "签订日期（信息类型：日期）");
        assert_eq!(
            col3["properties"]["value"]["description"],
            "规整后的提取值（日期，统一输出 YYYY-MM-DD）；找不到则为空字符串"
        );
        // 直出版同样生效
        let d = build_direct_schema(&cols);
        assert_eq!(
            d["properties"]["col_2"]["description"],
            "金额（信息类型：数字；定义：指甲乙双方约定的含税总金额）"
        );
    }
}
