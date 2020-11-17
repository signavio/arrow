// Licensed to the Apache Software Foundation (ASF) under one
// or more contributor license agreements.  See the NOTICE file
// distributed with this work for additional information
// regarding copyright ownership.  The ASF licenses this file
// to you under the Apache License, Version 2.0 (the
// "License"); you may not use this file except in compliance
// with the License.  You may obtain a copy of the License at
//
//   http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing,
// software distributed under the License is distributed on an
// "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
// KIND, either express or implied.  See the License for the
// specific language governing permissions and limitations
// under the License.

use std::convert::TryFrom;
use std::env;
use std::sync::Arc;

extern crate arrow;
extern crate datafusion;

use arrow::{array::*, datatypes::TimeUnit};
use arrow::{datatypes::Int32Type, datatypes::Int64Type, record_batch::RecordBatch};
use arrow::{
    datatypes::{DataType, Field, Schema, SchemaRef},
    util::display::array_value_to_string,
};

use datafusion::datasource::{csv::CsvReadOptions, MemTable};
use datafusion::error::Result;
use datafusion::execution::context::ExecutionContext;
use datafusion::logical_plan::LogicalPlan;
use datafusion::prelude::create_udf;

#[tokio::test]
async fn nyc() -> Result<()> {
    // schema for nyxtaxi csv files
    let schema = Schema::new(vec![
        Field::new("VendorID", DataType::Utf8, true),
        Field::new("tpep_pickup_datetime", DataType::Utf8, true),
        Field::new("tpep_dropoff_datetime", DataType::Utf8, true),
        Field::new("passenger_count", DataType::Utf8, true),
        Field::new("trip_distance", DataType::Float64, true),
        Field::new("RatecodeID", DataType::Utf8, true),
        Field::new("store_and_fwd_flag", DataType::Utf8, true),
        Field::new("PULocationID", DataType::Utf8, true),
        Field::new("DOLocationID", DataType::Utf8, true),
        Field::new("payment_type", DataType::Utf8, true),
        Field::new("fare_amount", DataType::Float64, true),
        Field::new("extra", DataType::Float64, true),
        Field::new("mta_tax", DataType::Float64, true),
        Field::new("tip_amount", DataType::Float64, true),
        Field::new("tolls_amount", DataType::Float64, true),
        Field::new("improvement_surcharge", DataType::Float64, true),
        Field::new("total_amount", DataType::Float64, true),
    ]);

    let mut ctx = ExecutionContext::new();
    ctx.register_csv(
        "tripdata",
        "file.csv",
        CsvReadOptions::new().schema(&schema),
    )?;

    let logical_plan = ctx.create_logical_plan(
        "SELECT passenger_count, MIN(fare_amount), MAX(fare_amount) \
         FROM tripdata GROUP BY passenger_count",
    )?;

    let optimized_plan = ctx.optimize(&logical_plan)?;

    match &optimized_plan {
        LogicalPlan::Aggregate { input, .. } => match input.as_ref() {
            LogicalPlan::TableScan {
                ref projected_schema,
                ..
            } => {
                assert_eq!(2, projected_schema.fields().len());
                assert_eq!(projected_schema.field(0).name(), "passenger_count");
                assert_eq!(projected_schema.field(1).name(), "fare_amount");
            }
            _ => assert!(false),
        },
        _ => assert!(false),
    }

    Ok(())
}

#[tokio::test]
async fn parquet_query() {
    let mut ctx = ExecutionContext::new();
    register_alltypes_parquet(&mut ctx);
    // NOTE that string_col is actually a binary column and does not have the UTF8 logical type
    // so we need an explicit cast
    let sql = "SELECT id, CAST(string_col AS varchar) FROM alltypes_plain";
    let actual = execute(&mut ctx, sql).await;
    let expected = vec![
        vec!["4", "0"],
        vec!["5", "1"],
        vec!["6", "0"],
        vec!["7", "1"],
        vec!["2", "0"],
        vec!["3", "1"],
        vec!["0", "0"],
        vec!["1", "1"],
    ];

    assert_eq!(expected, actual);
}

#[tokio::test]
async fn parquet_single_nan_schema() {
    let mut ctx = ExecutionContext::new();
    let testdata = env::var("PARQUET_TEST_DATA").expect("PARQUET_TEST_DATA not defined");
    ctx.register_parquet("single_nan", &format!("{}/single_nan.parquet", testdata))
        .unwrap();
    let sql = "SELECT mycol FROM single_nan";
    let plan = ctx.create_logical_plan(&sql).unwrap();
    let plan = ctx.optimize(&plan).unwrap();
    let plan = ctx.create_physical_plan(&plan).unwrap();
    let results = ctx.collect(plan).await.unwrap();
    for batch in results {
        assert_eq!(1, batch.num_rows());
        assert_eq!(1, batch.num_columns());
    }
}

#[tokio::test]
async fn parquet_list_columns() {
    let mut ctx = ExecutionContext::new();
    let testdata = env::var("PARQUET_TEST_DATA").expect("PARQUET_TEST_DATA not defined");
    ctx.register_parquet(
        "list_columns",
        &format!("{}/list_columns.parquet", testdata),
    )
    .unwrap();

    let schema = Arc::new(Schema::new(vec![
        Field::new(
            "int64_list",
            DataType::List(Box::new(DataType::Int64)),
            true,
        ),
        Field::new("utf8_list", DataType::List(Box::new(DataType::Utf8)), true),
    ]));

    let sql = "SELECT int64_list, utf8_list FROM list_columns";
    let plan = ctx.create_logical_plan(&sql).unwrap();
    let plan = ctx.optimize(&plan).unwrap();
    let plan = ctx.create_physical_plan(&plan).unwrap();
    let results = ctx.collect(plan).await.unwrap();

    //   int64_list              utf8_list
    // 0  [1, 2, 3]        [abc, efg, hij]
    // 1  [None, 1]                   None
    // 2        [4]  [efg, None, hij, xyz]

    assert_eq!(1, results.len());
    let batch = &results[0];
    assert_eq!(3, batch.num_rows());
    assert_eq!(2, batch.num_columns());
    assert_eq!(schema, batch.schema());

    let int_list_array = batch
        .column(0)
        .as_any()
        .downcast_ref::<ListArray>()
        .unwrap();
    let utf8_list_array = batch
        .column(1)
        .as_any()
        .downcast_ref::<ListArray>()
        .unwrap();

    assert_eq!(
        int_list_array
            .value(0)
            .as_any()
            .downcast_ref::<PrimitiveArray<Int64Type>>()
            .unwrap(),
        &PrimitiveArray::<Int64Type>::from(vec![Some(1), Some(2), Some(3),])
    );

    assert_eq!(
        utf8_list_array
            .value(0)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap(),
        &StringArray::try_from(vec![Some("abc"), Some("efg"), Some("hij"),]).unwrap()
    );

    assert_eq!(
        int_list_array
            .value(1)
            .as_any()
            .downcast_ref::<PrimitiveArray<Int64Type>>()
            .unwrap(),
        &PrimitiveArray::<Int64Type>::from(vec![None, Some(1),])
    );

    assert!(utf8_list_array.is_null(1));

    assert_eq!(
        int_list_array
            .value(2)
            .as_any()
            .downcast_ref::<PrimitiveArray<Int64Type>>()
            .unwrap(),
        &PrimitiveArray::<Int64Type>::from(vec![Some(4),])
    );

    let result = utf8_list_array.value(2);
    let result = result.as_any().downcast_ref::<StringArray>().unwrap();

    assert_eq!(result.value(0), "efg");
    assert!(result.is_null(1));
    assert_eq!(result.value(2), "hij");
    assert_eq!(result.value(3), "xyz");
}

#[tokio::test]
async fn csv_count_star() -> Result<()> {
    let mut ctx = ExecutionContext::new();
    register_aggregate_csv(&mut ctx)?;
    let sql = "SELECT COUNT(*), COUNT(1) AS c, COUNT(c1) FROM aggregate_test_100";
    let actual = execute(&mut ctx, sql).await;
    let expected = vec![vec!["100", "100", "100"]];
    assert_eq!(expected, actual);
    Ok(())
}

#[tokio::test]
async fn csv_query_with_predicate() -> Result<()> {
    let mut ctx = ExecutionContext::new();
    register_aggregate_csv(&mut ctx)?;
    let sql = "SELECT c1, c12 FROM aggregate_test_100 WHERE c12 > 0.376 AND c12 < 0.4";
    let actual = execute(&mut ctx, sql).await;
    let expected = vec![
        vec!["e", "0.39144436569161134"],
        vec!["d", "0.38870280983958583"],
    ];
    assert_eq!(expected, actual);
    Ok(())
}

#[tokio::test]
async fn csv_query_with_negated_predicate() -> Result<()> {
    let mut ctx = ExecutionContext::new();
    register_aggregate_csv(&mut ctx)?;
    let sql = "SELECT COUNT(1) FROM aggregate_test_100 WHERE NOT(c1 != 'a')";
    let actual = execute(&mut ctx, sql).await;
    let expected = vec![vec!["21"]];
    assert_eq!(expected, actual);
    Ok(())
}

#[tokio::test]
async fn csv_query_with_is_not_null_predicate() -> Result<()> {
    let mut ctx = ExecutionContext::new();
    register_aggregate_csv(&mut ctx)?;
    let sql = "SELECT COUNT(1) FROM aggregate_test_100 WHERE c1 IS NOT NULL";
    let actual = execute(&mut ctx, sql).await;
    let expected = vec![vec!["100"]];
    assert_eq!(expected, actual);
    Ok(())
}

#[tokio::test]
async fn csv_query_with_is_null_predicate() -> Result<()> {
    let mut ctx = ExecutionContext::new();
    register_aggregate_csv(&mut ctx)?;
    let sql = "SELECT COUNT(1) FROM aggregate_test_100 WHERE c1 IS NULL";
    let actual = execute(&mut ctx, sql).await;
    let expected = vec![vec!["0"]];
    assert_eq!(expected, actual);
    Ok(())
}

#[tokio::test]
async fn csv_query_group_by_int_min_max() -> Result<()> {
    let mut ctx = ExecutionContext::new();
    register_aggregate_csv(&mut ctx)?;
    let sql = "SELECT c2, MIN(c12), MAX(c12) FROM aggregate_test_100 GROUP BY c2";
    let mut actual = execute(&mut ctx, sql).await;
    actual.sort();
    let expected = vec![
        vec!["1", "0.05636955101974106", "0.9965400387585364"],
        vec!["2", "0.16301110515739792", "0.991517828651004"],
        vec!["3", "0.047343434291126085", "0.9293883502480845"],
        vec!["4", "0.02182578039211991", "0.9237877978193884"],
        vec!["5", "0.01479305307777301", "0.9723580396501548"],
    ];
    assert_eq!(expected, actual);
    Ok(())
}

#[tokio::test]
async fn csv_query_group_by_two_columns() -> Result<()> {
    let mut ctx = ExecutionContext::new();
    register_aggregate_csv(&mut ctx)?;
    let sql = "SELECT c1, c2, MIN(c3) FROM aggregate_test_100 GROUP BY c1, c2";
    let mut actual = execute(&mut ctx, sql).await;
    actual.sort();
    let expected = vec![
        vec!["a", "1", "-85"],
        vec!["a", "2", "-48"],
        vec!["a", "3", "-72"],
        vec!["a", "4", "-101"],
        vec!["a", "5", "-101"],
        vec!["b", "1", "12"],
        vec!["b", "2", "-60"],
        vec!["b", "3", "-101"],
        vec!["b", "4", "-117"],
        vec!["b", "5", "-82"],
        vec!["c", "1", "-24"],
        vec!["c", "2", "-117"],
        vec!["c", "3", "-2"],
        vec!["c", "4", "-90"],
        vec!["c", "5", "-94"],
        vec!["d", "1", "-99"],
        vec!["d", "2", "93"],
        vec!["d", "3", "-76"],
        vec!["d", "4", "5"],
        vec!["d", "5", "-59"],
        vec!["e", "1", "36"],
        vec!["e", "2", "-61"],
        vec!["e", "3", "-95"],
        vec!["e", "4", "-56"],
        vec!["e", "5", "-86"],
    ];
    assert_eq!(expected, actual);
    Ok(())
}

#[tokio::test]
async fn csv_query_avg_sqrt() -> Result<()> {
    let mut ctx = create_ctx()?;
    register_aggregate_csv(&mut ctx)?;
    let sql = "SELECT avg(custom_sqrt(c12)) FROM aggregate_test_100";
    let mut actual = execute(&mut ctx, sql).await;
    actual.sort();
    let expected = vec![vec!["0.6706002946036462"]];
    assert_eq!(actual, expected);
    Ok(())
}

/// test that casting happens on udfs.
/// c11 is f32, but `custom_sqrt` requires f64. Casting happens but the logical plan and
/// physical plan have the same schema.
#[tokio::test]
async fn csv_query_custom_udf_with_cast() -> Result<()> {
    let mut ctx = create_ctx()?;
    register_aggregate_csv(&mut ctx)?;
    let sql = "SELECT avg(custom_sqrt(c11)) FROM aggregate_test_100";
    let actual = execute(&mut ctx, sql).await;
    let expected = vec![vec!["0.6584408483418833"]];
    assert_eq!(actual, expected);
    Ok(())
}

/// sqrt(f32) is slightly different than sqrt(CAST(f32 AS double)))
#[tokio::test]
async fn sqrt_f32_vs_f64() -> Result<()> {
    let mut ctx = create_ctx()?;
    register_aggregate_csv(&mut ctx)?;
    // sqrt(f32)'s plan passes
    let sql = "SELECT avg(sqrt(c11)) FROM aggregate_test_100";
    let actual = execute(&mut ctx, sql).await;
    let expected = vec![vec!["0.6584408485889435"]];

    assert_eq!(actual, expected);
    let sql = "SELECT avg(sqrt(CAST(c11 AS double))) FROM aggregate_test_100";
    let actual = execute(&mut ctx, sql).await;
    let expected = vec![vec!["0.6584408483418833"]];
    assert_eq!(actual, expected);
    Ok(())
}

#[tokio::test]
async fn csv_query_error() -> Result<()> {
    // sin(utf8) should error
    let mut ctx = create_ctx()?;
    register_aggregate_csv(&mut ctx)?;
    let sql = "SELECT sin(c1) FROM aggregate_test_100";
    let plan = ctx.create_logical_plan(&sql);
    assert!(plan.is_err());
    Ok(())
}

// this query used to deadlock due to the call udf(udf())
#[tokio::test]
async fn csv_query_sqrt_sqrt() -> Result<()> {
    let mut ctx = create_ctx()?;
    register_aggregate_csv(&mut ctx)?;
    let sql = "SELECT sqrt(sqrt(c12)) FROM aggregate_test_100 LIMIT 1";
    let actual = execute(&mut ctx, sql).await;
    // sqrt(sqrt(c12=0.9294097332465232)) = 0.9818650561397431
    let expected = vec![vec!["0.9818650561397431"]];
    assert_eq!(actual, expected);
    Ok(())
}

fn create_ctx() -> Result<ExecutionContext> {
    let mut ctx = ExecutionContext::new();

    // register a custom UDF
    ctx.register_udf(create_udf(
        "custom_sqrt",
        vec![DataType::Float64],
        Arc::new(DataType::Float64),
        Arc::new(custom_sqrt),
    ));

    Ok(ctx)
}

fn custom_sqrt(args: &[ArrayRef]) -> Result<ArrayRef> {
    let input = &args[0]
        .as_any()
        .downcast_ref::<Float64Array>()
        .expect("cast failed");

    let mut builder = Float64Builder::new(input.len());
    for i in 0..input.len() {
        if input.is_null(i) {
            builder.append_null()?;
        } else {
            builder.append_value(input.value(i).sqrt())?;
        }
    }
    Ok(Arc::new(builder.finish()))
}

#[tokio::test]
async fn csv_query_avg() -> Result<()> {
    let mut ctx = ExecutionContext::new();
    register_aggregate_csv(&mut ctx)?;
    let sql = "SELECT avg(c12) FROM aggregate_test_100";
    let mut actual = execute(&mut ctx, sql).await;
    actual.sort();
    let expected = vec![vec!["0.5089725099127211"]];
    assert_eq!(expected, actual);
    Ok(())
}

#[tokio::test]
async fn csv_query_group_by_avg() -> Result<()> {
    let mut ctx = ExecutionContext::new();
    register_aggregate_csv(&mut ctx)?;
    let sql = "SELECT c1, avg(c12) FROM aggregate_test_100 GROUP BY c1";
    let mut actual = execute(&mut ctx, sql).await;
    actual.sort();
    let expected = vec![
        vec!["a", "0.48754517466109415"],
        vec!["b", "0.41040709263815384"],
        vec!["c", "0.6600456536439784"],
        vec!["d", "0.48855379387549824"],
        vec!["e", "0.48600669271341534"],
    ];
    assert_eq!(expected, actual);
    Ok(())
}

#[tokio::test]
async fn csv_query_group_by_avg_with_projection() -> Result<()> {
    let mut ctx = ExecutionContext::new();
    register_aggregate_csv(&mut ctx)?;
    let sql = "SELECT avg(c12), c1 FROM aggregate_test_100 GROUP BY c1";
    let mut actual = execute(&mut ctx, sql).await;
    actual.sort();
    let expected = vec![
        vec!["0.41040709263815384", "b"],
        vec!["0.48600669271341534", "e"],
        vec!["0.48754517466109415", "a"],
        vec!["0.48855379387549824", "d"],
        vec!["0.6600456536439784", "c"],
    ];
    assert_eq!(expected, actual);
    Ok(())
}

#[tokio::test]
async fn csv_query_avg_multi_batch() -> Result<()> {
    let mut ctx = ExecutionContext::new();
    register_aggregate_csv(&mut ctx)?;
    let sql = "SELECT avg(c12) FROM aggregate_test_100";
    let plan = ctx.create_logical_plan(&sql).unwrap();
    let plan = ctx.optimize(&plan).unwrap();
    let plan = ctx.create_physical_plan(&plan).unwrap();
    let results = ctx.collect(plan).await.unwrap();
    let batch = &results[0];
    let column = batch.column(0);
    let array = column.as_any().downcast_ref::<Float64Array>().unwrap();
    let actual = array.value(0);
    let expected = 0.5089725;
    // Due to float number's accuracy, different batch size will lead to different
    // answers.
    assert!((expected - actual).abs() < 0.01);
    Ok(())
}

#[tokio::test]
async fn csv_query_count() -> Result<()> {
    let mut ctx = ExecutionContext::new();
    register_aggregate_csv(&mut ctx)?;
    let sql = "SELECT count(c12) FROM aggregate_test_100";
    let actual = execute(&mut ctx, sql).await;
    let expected = vec![vec!["100"]];
    assert_eq!(expected, actual);
    Ok(())
}

#[tokio::test]
async fn csv_query_group_by_int_count() -> Result<()> {
    let mut ctx = ExecutionContext::new();
    register_aggregate_csv(&mut ctx)?;
    let sql = "SELECT c1, count(c12) FROM aggregate_test_100 GROUP BY c1";
    let mut actual = execute(&mut ctx, sql).await;
    actual.sort();
    let expected = vec![
        vec!["a", "21"],
        vec!["b", "19"],
        vec!["c", "21"],
        vec!["d", "18"],
        vec!["e", "21"],
    ];
    assert_eq!(expected, actual);
    Ok(())
}

#[tokio::test]
async fn csv_query_group_with_aliased_aggregate() -> Result<()> {
    let mut ctx = ExecutionContext::new();
    register_aggregate_csv(&mut ctx)?;
    let sql = "SELECT c1, count(c12) AS count FROM aggregate_test_100 GROUP BY c1";
    let mut actual = execute(&mut ctx, sql).await;
    actual.sort();
    let expected = vec![
        vec!["a", "21"],
        vec!["b", "19"],
        vec!["c", "21"],
        vec!["d", "18"],
        vec!["e", "21"],
    ];
    assert_eq!(expected, actual);
    Ok(())
}

#[tokio::test]
async fn csv_query_group_by_string_min_max() -> Result<()> {
    let mut ctx = ExecutionContext::new();
    register_aggregate_csv(&mut ctx)?;
    let sql = "SELECT c1, MIN(c12), MAX(c12) FROM aggregate_test_100 GROUP BY c1";
    let mut actual = execute(&mut ctx, sql).await;
    actual.sort();
    let expected = vec![
        vec!["a", "0.02182578039211991", "0.9800193410444061"],
        vec!["b", "0.04893135681998029", "0.9185813970744787"],
        vec!["c", "0.0494924465469434", "0.991517828651004"],
        vec!["d", "0.061029375346466685", "0.9748360509016578"],
        vec!["e", "0.01479305307777301", "0.9965400387585364"],
    ];
    assert_eq!(expected, actual);
    Ok(())
}

#[tokio::test]
async fn csv_query_cast() -> Result<()> {
    let mut ctx = ExecutionContext::new();
    register_aggregate_csv(&mut ctx)?;
    let sql = "SELECT CAST(c12 AS float) FROM aggregate_test_100 WHERE c12 > 0.376 AND c12 < 0.4";
    let actual = execute(&mut ctx, sql).await;
    let expected = vec![vec!["0.39144436569161134"], vec!["0.38870280983958583"]];
    assert_eq!(expected, actual);
    Ok(())
}

#[tokio::test]
async fn csv_query_cast_literal() -> Result<()> {
    let mut ctx = ExecutionContext::new();
    register_aggregate_csv(&mut ctx)?;
    let sql = "SELECT c12, CAST(1 AS float) FROM aggregate_test_100 WHERE c12 > CAST(0 AS float) LIMIT 2";
    let actual = execute(&mut ctx, sql).await;
    let expected = vec![
        vec!["0.9294097332465232", "1"],
        vec!["0.3114712539863804", "1"],
    ];
    assert_eq!(expected, actual);
    Ok(())
}

#[tokio::test]
async fn csv_query_limit() -> Result<()> {
    let mut ctx = ExecutionContext::new();
    register_aggregate_csv(&mut ctx)?;
    let sql = "SELECT c1 FROM aggregate_test_100 LIMIT 2";
    let actual = execute(&mut ctx, sql).await;
    let expected = vec![vec!["c"], vec!["d"]];
    assert_eq!(expected, actual);
    Ok(())
}

#[tokio::test]
async fn csv_query_limit_bigger_than_nbr_of_rows() -> Result<()> {
    let mut ctx = ExecutionContext::new();
    register_aggregate_csv(&mut ctx)?;
    let sql = "SELECT c2 FROM aggregate_test_100 LIMIT 200";
    let actual = execute(&mut ctx, sql).await;
    let expected = vec![
        vec!["2"],
        vec!["5"],
        vec!["1"],
        vec!["1"],
        vec!["5"],
        vec!["4"],
        vec!["3"],
        vec!["3"],
        vec!["1"],
        vec!["4"],
        vec!["1"],
        vec!["4"],
        vec!["3"],
        vec!["2"],
        vec!["1"],
        vec!["1"],
        vec!["2"],
        vec!["1"],
        vec!["3"],
        vec!["2"],
        vec!["4"],
        vec!["1"],
        vec!["5"],
        vec!["4"],
        vec!["2"],
        vec!["1"],
        vec!["4"],
        vec!["5"],
        vec!["2"],
        vec!["3"],
        vec!["4"],
        vec!["2"],
        vec!["1"],
        vec!["5"],
        vec!["3"],
        vec!["1"],
        vec!["2"],
        vec!["3"],
        vec!["3"],
        vec!["3"],
        vec!["2"],
        vec!["4"],
        vec!["1"],
        vec!["3"],
        vec!["2"],
        vec!["5"],
        vec!["2"],
        vec!["1"],
        vec!["4"],
        vec!["1"],
        vec!["4"],
        vec!["2"],
        vec!["5"],
        vec!["4"],
        vec!["2"],
        vec!["3"],
        vec!["4"],
        vec!["4"],
        vec!["4"],
        vec!["5"],
        vec!["4"],
        vec!["2"],
        vec!["1"],
        vec!["2"],
        vec!["4"],
        vec!["2"],
        vec!["3"],
        vec!["5"],
        vec!["1"],
        vec!["1"],
        vec!["4"],
        vec!["2"],
        vec!["1"],
        vec!["2"],
        vec!["1"],
        vec!["1"],
        vec!["5"],
        vec!["4"],
        vec!["5"],
        vec!["2"],
        vec!["3"],
        vec!["2"],
        vec!["4"],
        vec!["1"],
        vec!["3"],
        vec!["4"],
        vec!["3"],
        vec!["2"],
        vec!["5"],
        vec!["3"],
        vec!["3"],
        vec!["2"],
        vec!["5"],
        vec!["5"],
        vec!["4"],
        vec!["1"],
        vec!["3"],
        vec!["3"],
        vec!["4"],
        vec!["4"],
    ];
    assert_eq!(expected, actual);
    Ok(())
}

#[tokio::test]
async fn csv_query_limit_with_same_nbr_of_rows() -> Result<()> {
    let mut ctx = ExecutionContext::new();
    register_aggregate_csv(&mut ctx)?;
    let sql = "SELECT c2 FROM aggregate_test_100 LIMIT 100";
    let actual = execute(&mut ctx, sql).await;
    let expected = vec![
        vec!["2"],
        vec!["5"],
        vec!["1"],
        vec!["1"],
        vec!["5"],
        vec!["4"],
        vec!["3"],
        vec!["3"],
        vec!["1"],
        vec!["4"],
        vec!["1"],
        vec!["4"],
        vec!["3"],
        vec!["2"],
        vec!["1"],
        vec!["1"],
        vec!["2"],
        vec!["1"],
        vec!["3"],
        vec!["2"],
        vec!["4"],
        vec!["1"],
        vec!["5"],
        vec!["4"],
        vec!["2"],
        vec!["1"],
        vec!["4"],
        vec!["5"],
        vec!["2"],
        vec!["3"],
        vec!["4"],
        vec!["2"],
        vec!["1"],
        vec!["5"],
        vec!["3"],
        vec!["1"],
        vec!["2"],
        vec!["3"],
        vec!["3"],
        vec!["3"],
        vec!["2"],
        vec!["4"],
        vec!["1"],
        vec!["3"],
        vec!["2"],
        vec!["5"],
        vec!["2"],
        vec!["1"],
        vec!["4"],
        vec!["1"],
        vec!["4"],
        vec!["2"],
        vec!["5"],
        vec!["4"],
        vec!["2"],
        vec!["3"],
        vec!["4"],
        vec!["4"],
        vec!["4"],
        vec!["5"],
        vec!["4"],
        vec!["2"],
        vec!["1"],
        vec!["2"],
        vec!["4"],
        vec!["2"],
        vec!["3"],
        vec!["5"],
        vec!["1"],
        vec!["1"],
        vec!["4"],
        vec!["2"],
        vec!["1"],
        vec!["2"],
        vec!["1"],
        vec!["1"],
        vec!["5"],
        vec!["4"],
        vec!["5"],
        vec!["2"],
        vec!["3"],
        vec!["2"],
        vec!["4"],
        vec!["1"],
        vec!["3"],
        vec!["4"],
        vec!["3"],
        vec!["2"],
        vec!["5"],
        vec!["3"],
        vec!["3"],
        vec!["2"],
        vec!["5"],
        vec!["5"],
        vec!["4"],
        vec!["1"],
        vec!["3"],
        vec!["3"],
        vec!["4"],
        vec!["4"],
    ];
    assert_eq!(expected, actual);
    Ok(())
}

#[tokio::test]
async fn csv_query_limit_zero() -> Result<()> {
    let mut ctx = ExecutionContext::new();
    register_aggregate_csv(&mut ctx)?;
    let sql = "SELECT c1 FROM aggregate_test_100 LIMIT 0";
    let actual = execute(&mut ctx, sql).await;
    let expected: Vec<Vec<String>> = vec![];
    assert_eq!(expected, actual);
    Ok(())
}

#[tokio::test]
async fn csv_query_create_external_table() {
    let mut ctx = ExecutionContext::new();
    register_aggregate_csv_by_sql(&mut ctx).await;
    let sql = "SELECT c1, c2, c3, c4, c5, c6, c7, c8, c9, 10, c11, c12, c13 FROM aggregate_test_100 LIMIT 1";
    let actual = execute(&mut ctx, sql).await;
    let expected = vec![vec![
        "c",
        "2",
        "1",
        "18109",
        "2033001162",
        "-6513304855495910254",
        "25",
        "43062",
        "1491205016",
        "10",
        "0.110830784",
        "0.9294097332465232",
        "6WfVFBVGJSQb7FhA7E0lBwdvjfZnSW",
    ]];
    assert_eq!(expected, actual);
}

#[tokio::test]
async fn csv_query_external_table_count() {
    let mut ctx = ExecutionContext::new();
    register_aggregate_csv_by_sql(&mut ctx).await;
    let sql = "SELECT COUNT(c12) FROM aggregate_test_100";
    let actual = execute(&mut ctx, sql).await;
    let expected = vec![vec!["100"]];
    assert_eq!(expected, actual);
}

#[tokio::test]
async fn csv_query_external_table_sum() {
    let mut ctx = ExecutionContext::new();
    // cast smallint and int to bigint to avoid overflow during calculation
    register_aggregate_csv_by_sql(&mut ctx).await;
    let sql =
        "SELECT SUM(CAST(c7 AS BIGINT)), SUM(CAST(c8 AS BIGINT)) FROM aggregate_test_100";
    let actual = execute(&mut ctx, sql).await;
    let expected = vec![vec!["13060", "3017641"]];
    assert_eq!(expected, actual);
}

#[tokio::test]
async fn csv_query_count_star() {
    let mut ctx = ExecutionContext::new();
    register_aggregate_csv_by_sql(&mut ctx).await;
    let sql = "SELECT COUNT(*) FROM aggregate_test_100";
    let actual = execute(&mut ctx, sql).await;
    let expected = vec![vec!["100"]];
    assert_eq!(expected, actual);
}

#[tokio::test]
async fn csv_query_count_one() {
    let mut ctx = ExecutionContext::new();
    register_aggregate_csv_by_sql(&mut ctx).await;
    let sql = "SELECT COUNT(1) FROM aggregate_test_100";
    let actual = execute(&mut ctx, sql).await;
    let expected = vec![vec!["100"]];
    assert_eq!(expected, actual);
}

#[tokio::test]
async fn csv_explain() {
    let mut ctx = ExecutionContext::new();
    register_aggregate_csv_by_sql(&mut ctx).await;
    let sql = "EXPLAIN SELECT c1 FROM aggregate_test_100 where c2 > 10";
    let actual = execute(&mut ctx, sql).await;
    let expected = vec![
        vec![
            "logical_plan",
            "Projection: #c1\n  Filter: #c2 Gt Int64(10)\n    TableScan: aggregate_test_100 projection=None"
        ]
    ];
    assert_eq!(expected, actual);

    // Also, expect same result with lowercase explain
    let sql = "explain SELECT c1 FROM aggregate_test_100 where c2 > 10";
    let actual = execute(&mut ctx, sql).await;
    assert_eq!(expected, actual);
}

#[tokio::test]
async fn csv_explain_verbose() {
    let mut ctx = ExecutionContext::new();
    register_aggregate_csv_by_sql(&mut ctx).await;
    let sql = "EXPLAIN VERBOSE SELECT c1 FROM aggregate_test_100 where c2 > 10";
    let actual = execute(&mut ctx, sql).await;

    // flatten to a single string
    let actual = actual.into_iter().map(|r| r.join("\t")).collect::<String>();

    // Don't actually test the contents of the debuging output (as
    // that may change and keeping this test updated will be a
    // pain). Instead just check for a few key pieces.
    assert!(actual.contains("logical_plan"), "Actual: '{}'", actual);
    assert!(actual.contains("physical_plan"), "Actual: '{}'", actual);
    assert!(actual.contains("#c2 Gt Int64(10)"), "Actual: '{}'", actual);
}

fn aggr_test_schema() -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("c1", DataType::Utf8, false),
        Field::new("c2", DataType::UInt32, false),
        Field::new("c3", DataType::Int8, false),
        Field::new("c4", DataType::Int16, false),
        Field::new("c5", DataType::Int32, false),
        Field::new("c6", DataType::Int64, false),
        Field::new("c7", DataType::UInt8, false),
        Field::new("c8", DataType::UInt16, false),
        Field::new("c9", DataType::UInt32, false),
        Field::new("c10", DataType::UInt64, false),
        Field::new("c11", DataType::Float32, false),
        Field::new("c12", DataType::Float64, false),
        Field::new("c13", DataType::Utf8, false),
    ]))
}

async fn register_aggregate_csv_by_sql(ctx: &mut ExecutionContext) {
    let testdata = env::var("ARROW_TEST_DATA").expect("ARROW_TEST_DATA not defined");

    // TODO: The following c9 should be migrated to UInt32 and c10 should be UInt64 once
    // unsigned is supported.
    let df = ctx
        .sql(&format!(
            "
    CREATE EXTERNAL TABLE aggregate_test_100 (
        c1  VARCHAR NOT NULL,
        c2  INT NOT NULL,
        c3  SMALLINT NOT NULL,
        c4  SMALLINT NOT NULL,
        c5  INT NOT NULL,
        c6  BIGINT NOT NULL,
        c7  SMALLINT NOT NULL,
        c8  INT NOT NULL,
        c9  BIGINT NOT NULL,
        c10 VARCHAR NOT NULL,
        c11 FLOAT NOT NULL,
        c12 DOUBLE NOT NULL,
        c13 VARCHAR NOT NULL
    )
    STORED AS CSV
    WITH HEADER ROW
    LOCATION '{}/csv/aggregate_test_100.csv'
    ",
            testdata
        ))
        .expect("Creating dataframe for CREATE EXTERNAL TABLE");

    // Mimic the CLI and execute the resulting plan -- even though it
    // is effectively a no-op (returns zero rows)
    let results = df.collect().await.expect("Executing CREATE EXTERNAL TABLE");
    assert!(
        results.is_empty(),
        "Expected no rows from executing CREATE EXTERNAL TABLE"
    );
}

fn register_aggregate_csv(ctx: &mut ExecutionContext) -> Result<()> {
    let testdata = env::var("ARROW_TEST_DATA").expect("ARROW_TEST_DATA not defined");
    let schema = aggr_test_schema();
    ctx.register_csv(
        "aggregate_test_100",
        &format!("{}/csv/aggregate_test_100.csv", testdata),
        CsvReadOptions::new().schema(&schema),
    )?;
    Ok(())
}

fn register_alltypes_parquet(ctx: &mut ExecutionContext) {
    let testdata = env::var("PARQUET_TEST_DATA").expect("PARQUET_TEST_DATA not defined");
    ctx.register_parquet(
        "alltypes_plain",
        &format!("{}/alltypes_plain.parquet", testdata),
    )
    .unwrap();
}

/// Execute query and return result set as 2-d table of Vecs
/// `result[row][column]`
async fn execute(ctx: &mut ExecutionContext, sql: &str) -> Vec<Vec<String>> {
    let msg = format!("Creating logical plan for '{}'", sql);
    let plan = ctx.create_logical_plan(&sql).expect(&msg);
    let logical_schema = plan.schema();

    let msg = format!("Optimizing logical plan for '{}': {:?}", sql, plan);
    let plan = ctx.optimize(&plan).expect(&msg);
    let optimized_logical_schema = plan.schema();

    let msg = format!("Creating physical plan for '{}': {:?}", sql, plan);
    let plan = ctx.create_physical_plan(&plan).expect(&msg);
    let physical_schema = plan.schema();

    let msg = format!("Executing physical plan for '{}': {:?}", sql, plan);
    let results = ctx.collect(plan).await.expect(&msg);

    assert_eq!(logical_schema.as_ref(), optimized_logical_schema.as_ref());
    assert_eq!(logical_schema.as_ref(), physical_schema.as_ref());

    result_vec(&results)
}

/// Specialised String representation
fn col_str(column: &ArrayRef, row_index: usize) -> String {
    if column.is_null(row_index) {
        return "NULL".to_string();
    }

    // Special case ListArray as there is no pretty print support for it yet
    if let DataType::FixedSizeList(_, n) = column.data_type() {
        let array = column
            .as_any()
            .downcast_ref::<FixedSizeListArray>()
            .unwrap()
            .value(row_index);

        let mut r = Vec::with_capacity(*n as usize);
        for i in 0..*n {
            r.push(col_str(&array, i as usize));
        }
        return format!("[{}]", r.join(","));
    }

    array_value_to_string(column, row_index)
        .ok()
        .unwrap_or_else(|| "???".to_string())
}

/// Converts the results into a 2d array of strings, `result[row][column]`
/// Special cases nulls to NULL for testing
fn result_vec(results: &[RecordBatch]) -> Vec<Vec<String>> {
    let mut result = vec![];
    for batch in results {
        for row_index in 0..batch.num_rows() {
            let row_vec = batch
                .columns()
                .iter()
                .map(|column| col_str(column, row_index))
                .collect();
            result.push(row_vec);
        }
    }
    result
}

async fn generic_query_length<T: 'static + Array + From<Vec<&'static str>>>(
    datatype: DataType,
) -> Result<()> {
    let schema = Arc::new(Schema::new(vec![Field::new("c1", datatype, false)]));

    let data = RecordBatch::try_new(
        schema.clone(),
        vec![Arc::new(T::from(vec!["", "a", "aa", "aaa"]))],
    )?;

    let table = MemTable::new(schema, vec![vec![data]])?;

    let mut ctx = ExecutionContext::new();
    ctx.register_table("test", Box::new(table));
    let sql = "SELECT length(c1) FROM test";
    let actual = execute(&mut ctx, sql).await;
    let expected = vec![vec!["0"], vec!["1"], vec!["2"], vec!["3"]];
    assert_eq!(expected, actual);
    Ok(())
}

#[tokio::test]
async fn query_length() -> Result<()> {
    generic_query_length::<StringArray>(DataType::Utf8).await
}

#[tokio::test]
async fn query_large_length() -> Result<()> {
    generic_query_length::<LargeStringArray>(DataType::LargeUtf8).await
}

#[tokio::test]
async fn query_not() -> Result<()> {
    let schema = Arc::new(Schema::new(vec![Field::new("c1", DataType::Boolean, true)]));

    let data = RecordBatch::try_new(
        schema.clone(),
        vec![Arc::new(BooleanArray::from(vec![
            Some(false),
            None,
            Some(true),
        ]))],
    )?;

    let table = MemTable::new(schema, vec![vec![data]])?;

    let mut ctx = ExecutionContext::new();
    ctx.register_table("test", Box::new(table));
    let sql = "SELECT NOT c1 FROM test";
    let actual = execute(&mut ctx, sql).await;
    let expected = vec![vec!["true"], vec!["NULL"], vec!["false"]];
    assert_eq!(expected, actual);
    Ok(())
}

#[tokio::test]
async fn query_concat() -> Result<()> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("c1", DataType::Utf8, false),
        Field::new("c2", DataType::Int32, true),
    ]));

    let data = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(StringArray::from(vec!["", "a", "aa", "aaa"])),
            Arc::new(Int32Array::from(vec![Some(0), Some(1), None, Some(3)])),
        ],
    )?;

    let table = MemTable::new(schema, vec![vec![data]])?;

    let mut ctx = ExecutionContext::new();
    ctx.register_table("test", Box::new(table));
    let sql = "SELECT concat(c1, '-hi-', cast(c2 as varchar)) FROM test";
    let actual = execute(&mut ctx, sql).await;
    let expected = vec![
        vec!["-hi-0"],
        vec!["a-hi-1"],
        vec!["NULL"],
        vec!["aaa-hi-3"],
    ];
    assert_eq!(expected, actual);
    Ok(())
}

#[tokio::test]
async fn query_array() -> Result<()> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("c1", DataType::Utf8, false),
        Field::new("c2", DataType::Int32, true),
    ]));

    let data = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(StringArray::from(vec!["", "a", "aa", "aaa"])),
            Arc::new(Int32Array::from(vec![Some(0), Some(1), None, Some(3)])),
        ],
    )?;

    let table = MemTable::new(schema, vec![vec![data]])?;

    let mut ctx = ExecutionContext::new();
    ctx.register_table("test", Box::new(table));
    let sql = "SELECT array(c1, cast(c2 as varchar)) FROM test";
    let actual = execute(&mut ctx, sql).await;
    let expected = vec![
        vec!["[,0]"],
        vec!["[a,1]"],
        vec!["[aa,NULL]"],
        vec!["[aaa,3]"],
    ];
    assert_eq!(expected, actual);
    Ok(())
}

#[tokio::test]
async fn csv_query_sum_cast() {
    let mut ctx = ExecutionContext::new();
    register_aggregate_csv_by_sql(&mut ctx).await;
    // c8 = i32; c9 = i64
    let sql = "SELECT c8 + c9 FROM aggregate_test_100";
    // check that the physical and logical schemas are equal
    execute(&mut ctx, sql).await;
}

#[tokio::test]
async fn like() -> Result<()> {
    let mut ctx = ExecutionContext::new();
    register_aggregate_csv_by_sql(&mut ctx).await;
    let sql = "SELECT COUNT(c1) FROM aggregate_test_100 WHERE c13 LIKE '%FB%'";
    // check that the physical and logical schemas are equal
    let actual = execute(&mut ctx, sql).await;

    let expected = vec![vec!["1"]];
    assert_eq!(expected, actual);
    Ok(())
}

fn make_timestamp_nano_table() -> Result<Box<MemTable>> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("ts", DataType::Timestamp(TimeUnit::Nanosecond, None), false),
        Field::new("value", DataType::Int32, true),
    ]));

    let mut builder = TimestampNanosecondArray::builder(3);

    builder.append_value(1599572549190855000)?; // 2020-09-08T13:42:29.190855+00:00
    builder.append_value(1599568949190855000)?; // 2020-09-08T12:42:29.190855+00:00
    builder.append_value(1599565349190855000)?; // 2020-09-08T11:42:29.190855+00:00

    let data = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(builder.finish()),
            Arc::new(Int32Array::from(vec![Some(1), Some(2), Some(3)])),
        ],
    )?;
    let table = MemTable::new(schema, vec![vec![data]])?;
    Ok(Box::new(table))
}

#[tokio::test]
async fn to_timstamp() -> Result<()> {
    let mut ctx = ExecutionContext::new();
    ctx.register_table("ts_data", make_timestamp_nano_table()?);

    let sql = "SELECT COUNT(*) FROM ts_data where ts > to_timestamp('2020-09-08T12:00:00+00:00')";
    let actual = execute(&mut ctx, sql).await;

    let expected = vec![vec!["2"]];
    assert_eq!(expected, actual);
    Ok(())
}

#[tokio::test]
async fn query_is_null() -> Result<()> {
    let schema = Arc::new(Schema::new(vec![Field::new("c1", DataType::Float64, true)]));

    let data = RecordBatch::try_new(
        schema.clone(),
        vec![Arc::new(Float64Array::from(vec![
            Some(1.0),
            None,
            Some(f64::NAN),
        ]))],
    )?;

    let table = MemTable::new(schema, vec![vec![data]])?;

    let mut ctx = ExecutionContext::new();
    ctx.register_table("test", Box::new(table));
    let sql = "SELECT c1 IS NULL FROM test";
    let actual = execute(&mut ctx, sql).await;
    let expected = vec![vec!["false"], vec!["true"], vec!["false"]];
    assert_eq!(expected, actual);
    Ok(())
}

#[tokio::test]
async fn query_is_not_null() -> Result<()> {
    let schema = Arc::new(Schema::new(vec![Field::new("c1", DataType::Float64, true)]));

    let data = RecordBatch::try_new(
        schema.clone(),
        vec![Arc::new(Float64Array::from(vec![
            Some(1.0),
            None,
            Some(f64::NAN),
        ]))],
    )?;

    let table = MemTable::new(schema, vec![vec![data]])?;

    let mut ctx = ExecutionContext::new();
    ctx.register_table("test", Box::new(table));
    let sql = "SELECT c1 IS NOT NULL FROM test";
    let actual = execute(&mut ctx, sql).await;
    let expected = vec![vec!["true"], vec!["false"], vec!["true"]];

    assert_eq!(expected, actual);
    Ok(())
}

#[tokio::test]
async fn query_count_distinct() -> Result<()> {
    let schema = Arc::new(Schema::new(vec![Field::new("c1", DataType::Int32, true)]));

    let data = RecordBatch::try_new(
        schema.clone(),
        vec![Arc::new(Int32Array::from(vec![
            Some(0),
            Some(1),
            None,
            Some(3),
            Some(3),
        ]))],
    )?;

    let table = MemTable::new(schema, vec![vec![data]])?;

    let mut ctx = ExecutionContext::new();
    ctx.register_table("test", Box::new(table));
    let sql = "SELECT COUNT(DISTINCT c1) FROM test";
    let actual = execute(&mut ctx, sql).await;
    let expected = vec![vec!["3".to_string()]];
    assert_eq!(expected, actual);
    Ok(())
}

#[tokio::test]
async fn query_on_string_dictionary() -> Result<()> {
    // Test to ensure DataFusion can operate on dictionary types
    // Use StringDictionary (32 bit indexes = keys)
    let field_type =
        DataType::Dictionary(Box::new(DataType::Int32), Box::new(DataType::Utf8));
    let schema = Arc::new(Schema::new(vec![Field::new("d1", field_type, true)]));

    let keys_builder = PrimitiveBuilder::<Int32Type>::new(10);
    let values_builder = StringBuilder::new(10);
    let mut builder = StringDictionaryBuilder::new(keys_builder, values_builder);

    builder.append("one")?;
    builder.append_null()?;
    builder.append("three")?;
    let array = Arc::new(builder.finish());

    let data = RecordBatch::try_new(schema.clone(), vec![array])?;

    let table = MemTable::new(schema, vec![vec![data]])?;
    let mut ctx = ExecutionContext::new();
    ctx.register_table("test", Box::new(table));

    // Basic SELECT
    let sql = "SELECT * FROM test";
    let actual = execute(&mut ctx, sql).await;
    let expected = vec![vec!["one"], vec!["NULL"], vec!["three"]];
    assert_eq!(expected, actual);

    // basic filtering
    let sql = "SELECT * FROM test WHERE d1 IS NOT NULL";
    let actual = execute(&mut ctx, sql).await;
    let expected = vec![vec!["one"], vec!["three"]];
    assert_eq!(expected, actual);

    // filtering with constant
    let sql = "SELECT * FROM test WHERE d1 = 'three'";
    let actual = execute(&mut ctx, sql).await;
    let expected = vec![vec!["three"]];
    assert_eq!(expected, actual);

    // Expression evaluation
    let sql = "SELECT concat(d1, '-foo') FROM test";
    let actual = execute(&mut ctx, sql).await;
    let expected = vec![vec!["one-foo"], vec!["NULL"], vec!["three-foo"]];
    assert_eq!(expected, actual);

    // aggregation
    let sql = "SELECT COUNT(d1) FROM test";
    let actual = execute(&mut ctx, sql).await;
    let expected = vec![vec!["2"]];
    assert_eq!(expected, actual);

    Ok(())
}
