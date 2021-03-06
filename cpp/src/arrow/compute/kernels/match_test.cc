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

#include <algorithm>
#include <cstdint>
#include <cstdio>
#include <functional>
#include <locale>
#include <memory>
#include <stdexcept>
#include <string>
#include <vector>

#include <gtest/gtest.h>

#include "arrow/array.h"
#include "arrow/buffer.h"
#include "arrow/compute/context.h"
#include "arrow/compute/kernel.h"
#include "arrow/compute/kernels/match.h"
#include "arrow/compute/kernels/util_internal.h"
#include "arrow/compute/test_util.h"
#include "arrow/memory_pool.h"
#include "arrow/status.h"
#include "arrow/table.h"
#include "arrow/testing/gtest_common.h"
#include "arrow/testing/random.h"
#include "arrow/testing/util.h"
#include "arrow/type.h"
#include "arrow/type_traits.h"
#include "arrow/util/decimal.h"

namespace arrow {
namespace compute {

// ----------------------------------------------------------------------
// Match tests

class TestMatchKernel : public ComputeFixture, public TestBase {
 public:
  void CheckMatch(const std::shared_ptr<DataType>& type, const std::string& haystack_json,
                  const std::string& needles_json, const std::string& expected_json) {
    std::shared_ptr<Array> haystack = ArrayFromJSON(type, haystack_json);
    std::shared_ptr<Array> needles = ArrayFromJSON(type, needles_json);
    std::shared_ptr<Array> expected = ArrayFromJSON(int32(), expected_json);

    Datum actual_datum;
    ASSERT_OK(Match(&this->ctx_, haystack, needles, &actual_datum));
    std::shared_ptr<Array> actual = actual_datum.make_array();
    ASSERT_ARRAYS_EQUAL(*expected, *actual);
  }
};

template <typename Type>
class TestMatchKernelPrimitive : public TestMatchKernel {};

using PrimitiveDictionaries =
    ::testing::Types<Int8Type, UInt8Type, Int16Type, UInt16Type, Int32Type, UInt32Type,
                     Int64Type, UInt64Type, FloatType, DoubleType, Date32Type,
                     Date64Type>;

TYPED_TEST_SUITE(TestMatchKernelPrimitive, PrimitiveDictionaries);

TYPED_TEST(TestMatchKernelPrimitive, Match) {
  auto type = TypeTraits<TypeParam>::type_singleton();

  // No Nulls
  this->CheckMatch(type,
                   /* haystack= */ "[2, 1, 2, 1, 2, 3]",
                   /* needles= */ "[2, 1, 2, 3]",
                   /* expected= */ "[0, 1, 0, 1, 0, 2]");

  // Haystack array all null
  this->CheckMatch(type,
                   /* haystack= */ "[null, null, null, null, null, null]",
                   /* needles= */ "[2, 1, 3]",
                   /* expected= */ "[null, null, null, null, null, null]");

  // Needles array all null
  this->CheckMatch(type,
                   /* haystack= */ "[2, 1, 2, 1, 2, 3]",
                   /* needles= */ "[null, null, null, null]",
                   /* expected= */ "[null, null, null, null, null, null]");

  // Both arrays all null
  this->CheckMatch(type,
                   /* haystack= */ "[null, null, null, null]",
                   /* needles= */ "[null, null]",
                   /* expected= */ "[0, 0, 0, 0]");

  // No Match
  this->CheckMatch(type,
                   /* haystack= */ "[2, null, 7, 3, 8]",
                   /* needles= */ "[2, null, 2, null, 6, 3, 3]",
                   /* expected= */ "[0, 1, null, 3, null]");

  // Empty Arrays
  this->CheckMatch(type, "[]", "[]", "[]");
}

TYPED_TEST(TestMatchKernelPrimitive, PrimitiveResizeTable) {
  using T = typename TypeParam::c_type;

  const int64_t kTotalValues = std::min<int64_t>(INT16_MAX, 1UL << sizeof(T) / 2);
  const int64_t kRepeats = 5;

  Int32Builder expected_builder;
  NumericBuilder<TypeParam> haystack_builder;
  ASSERT_OK(expected_builder.Resize(kTotalValues * kRepeats));
  ASSERT_OK(haystack_builder.Resize(kTotalValues * kRepeats));

  for (int64_t i = 0; i < kTotalValues * kRepeats; i++) {
    const auto index = i % kTotalValues;

    haystack_builder.UnsafeAppend(static_cast<T>(index));
    expected_builder.UnsafeAppend(static_cast<int32_t>(index));
  }

  std::shared_ptr<Array> haystack, needles, expected;
  ASSERT_OK(haystack_builder.Finish(&haystack));
  needles = haystack;
  ASSERT_OK(expected_builder.Finish(&expected));

  Datum actual_datum;
  ASSERT_OK(Match(&this->ctx_, haystack, needles, &actual_datum));
  std::shared_ptr<Array> actual = actual_datum.make_array();
  ASSERT_ARRAYS_EQUAL(*expected, *actual);
}

TEST_F(TestMatchKernel, MatchNull) {
  CheckMatch(null(), "[null, null, null]", "[null, null]", "[0, 0, 0]");

  CheckMatch(null(), "[null, null, null]", "[]", "[null, null, null]");

  CheckMatch(null(), "[]", "[null, null]", "[]");

  CheckMatch(null(), "[]", "[]", "[]");
}

TEST_F(TestMatchKernel, MatchTimeTimestamp) {
  CheckMatch(time32(TimeUnit::SECOND),
             /* haystack= */ "[1, null, 5, 1, 2]",
             /* needles= */ "[2, 1, null, 1]",
             /* expected= */ "[1, 2, null, 1, 0]");

  // Needles array has no nulls
  CheckMatch(time32(TimeUnit::SECOND),
             /* haystack= */ "[2, null, 5, 1]",
             /* needles= */ "[2, 1, 1]",
             /* expected= */ "[0, null, null, 1]");

  // No match
  CheckMatch(time32(TimeUnit::SECOND), "[3, null, 5, 3]", "[2, 1, 2, 1, 2]",
             "[null, null, null, null]");

  // Empty arrays
  CheckMatch(time32(TimeUnit::SECOND), "[]", "[]", "[]");

  CheckMatch(time64(TimeUnit::NANO), "[2, null, 2, 1]", "[2, null, 1]", "[0, 1, 0, 2]");

  CheckMatch(timestamp(TimeUnit::NANO), "[2, null, 2, 1]", "[2, null, 2, 1]",
             "[0, 1, 0, 2]");

  // Empty haystack array
  CheckMatch(timestamp(TimeUnit::NANO), "[]", "[2, null, 2, 1]", "[]");

  // Empty needles array
  CheckMatch(timestamp(TimeUnit::NANO), "[2, null, 2, 1]", "[]",
             "[null, null, null, null]");

  // Both array are all null
  CheckMatch(time32(TimeUnit::SECOND), "[null, null, null, null]", "[null, null]",
             "[0, 0, 0, 0]");
}

TEST_F(TestMatchKernel, MatchBoolean) {
  CheckMatch(boolean(),
             /* haystack= */ "[false, null, false, true]",
             /* needles= */ "[null, false, true]",
             /* expected= */ "[1, 0, 1, 2]");

  CheckMatch(boolean(), "[false, null, false, true]", "[false, true, null, true, null]",
             "[0, 2, 0, 1]");

  // No Nulls
  CheckMatch(boolean(), "[true, true, false, true]", "[false, true]", "[1, 1, 0, 1]");

  CheckMatch(boolean(), "[false, true, false, true]", "[true, true, true, true]",
             "[null, 0, null, 0]");

  // No match
  CheckMatch(boolean(), "[true, true, true, true]", "[false, false, false]",
             "[null, null, null, null]");

  // Nulls in haystack array
  CheckMatch(boolean(), "[null, null, null, null]", "[true, true]",
             "[null, null, null, null]");

  // Nulls in needles array
  CheckMatch(boolean(), "[true, true, false, true]",
             "[null, null, null, null, null, null]", "[null, null, null, null]");

  // Both array have Nulls
  CheckMatch(boolean(), "[null, null, null, null]", "[null, null, null, null]",
             "[0, 0, 0, 0]");
}

template <typename Type>
class TestMatchKernelBinary : public TestMatchKernel {};

using BinaryTypes = ::testing::Types<BinaryType, StringType>;
TYPED_TEST_SUITE(TestMatchKernelBinary, BinaryTypes);

TYPED_TEST(TestMatchKernelBinary, MatchBinary) {
  auto type = TypeTraits<TypeParam>::type_singleton();
  this->CheckMatch(type, R"(["foo", null, "bar", "foo"])", R"(["foo", null, "bar"])",
                   R"([0, 1, 2, 0])");

  // No match
  this->CheckMatch(type,
                   /* haystack= */ R"(["foo", null, "bar", "foo"])",
                   /* needles= */ R"(["baz", "bazzz", "baz", "bazzz"])",
                   /* expected= */ R"([null, null, null, null])");

  // Nulls in haystack array
  this->CheckMatch(type,
                   /* haystack= */ R"([null, null, null, null])",
                   /* needles= */ R"(["foo", "bar", "foo"])",
                   /* expected= */ R"([null, null, null, null])");

  // Nulls in needles array
  this->CheckMatch(type, R"(["foo", "bar", "foo"])", R"([null, null, null])",
                   R"([null, null, null])");

  // Both array have Nulls
  this->CheckMatch(type,
                   /* haystack= */ R"([null, null, null, null])",
                   /* needles= */ R"([null, null, null, null])",
                   /* expected= */ R"([0, 0, 0, 0])");

  // Empty arrays
  this->CheckMatch(type, R"([])", R"([])", R"([])");

  // Empty haystack array
  this->CheckMatch(type, R"([])", R"(["foo", null, "bar", null])", "[]");

  // Empty needles array
  this->CheckMatch(type, R"(["foo", null, "bar", "foo"])", "[]",
                   R"([null, null, null, null])");
}

TEST_F(TestMatchKernel, BinaryResizeTable) {
  const int32_t kTotalValues = 10000;
#if !defined(ARROW_VALGRIND)
  const int32_t kRepeats = 10;
#else
  // Mitigate Valgrind's slowness
  const int32_t kRepeats = 3;
#endif

  const int32_t kBufSize = 20;

  Int32Builder expected_builder;
  StringBuilder haystack_builder;
  ASSERT_OK(expected_builder.Resize(kTotalValues * kRepeats));
  ASSERT_OK(haystack_builder.Resize(kTotalValues * kRepeats));
  ASSERT_OK(haystack_builder.ReserveData(kBufSize * kTotalValues * kRepeats));

  for (int32_t i = 0; i < kTotalValues * kRepeats; i++) {
    int32_t index = i % kTotalValues;

    char buf[kBufSize] = "test";
    ASSERT_GE(snprintf(buf + 4, sizeof(buf) - 4, "%d", index), 0);

    haystack_builder.UnsafeAppend(util::string_view(buf));
    expected_builder.UnsafeAppend(index);
  }

  std::shared_ptr<Array> haystack, needles, expected;
  ASSERT_OK(haystack_builder.Finish(&haystack));
  needles = haystack;
  ASSERT_OK(expected_builder.Finish(&expected));

  Datum actual_datum;
  ASSERT_OK(Match(&this->ctx_, haystack, needles, &actual_datum));
  std::shared_ptr<Array> actual = actual_datum.make_array();
  ASSERT_ARRAYS_EQUAL(*expected, *actual);
}

TEST_F(TestMatchKernel, MatchFixedSizeBinary) {
  CheckMatch(fixed_size_binary(5),
             /* haystack= */ R"(["bbbbb", null, "aaaaa", "ccccc"])",
             /* needles= */ R"(["bbbbb", null, "bbbbb", "aaaaa", "ccccc"])",
             /* expected= */ R"([0, 1, 2, 3])");

  // Nulls in haystack
  CheckMatch(fixed_size_binary(5),
             /* haystack= */ R"([null, null, null, null, null])",
             /* needles= */ R"(["bbbbb", "aabbb", "bbbbb", "aaaaa", "ccccc"])",
             /* expected= */ R"([null, null, null, null, null])");

  // Nulls in needles
  CheckMatch(fixed_size_binary(5),
             /* haystack= */ R"(["bbbbb", null, "bbbbb", "aaaaa", "ccccc"])",
             /* needles= */ R"([null, null, null])",
             /* expected= */ R"([null, 0, null, null, null])");

  // Both array have Nulls
  CheckMatch(fixed_size_binary(5),
             /* haystack= */ R"([null, null, null, null, null])",
             /* needles= */ R"([null, null, null, null])",
             /* expected= */ R"([0, 0, 0, 0, 0])");

  // No match
  CheckMatch(fixed_size_binary(5),
             /* haystack= */ R"(["bbbbc", "bbbbc", "aaaad", "cccca"])",
             /* needles= */ R"(["bbbbb", null, "bbbbb", "aaaaa", "ddddd"])",
             /* expected= */ R"([null, null, null, null])");

  // Empty haystack array
  CheckMatch(fixed_size_binary(5), R"([])",
             R"(["bbbbb", null, "bbbbb", "aaaaa", "ccccc"])", R"([])");

  // Empty needles array
  CheckMatch(fixed_size_binary(5), R"(["bbbbb", null, "bbbbb", "aaaaa", "ccccc"])",
             R"([])", R"([null, null, null, null, null])");

  // Empty arrays
  CheckMatch(fixed_size_binary(0), R"([])", R"([])", R"([])");
}

TEST_F(TestMatchKernel, MatchDecimal) {
  std::vector<Decimal128> input{12, 12, 11, 12};
  std::vector<Decimal128> member_set{12, 12, 11, 12};
  std::vector<int32_t> expected{0, 1, 2, 0};

  CheckMatch(decimal(2, 0),
             /* haystack= */ R"(["12", null, "11", "12"])",
             /* needles= */ R"(["12", null, "11", "12"])",
             /* expected= */ R"([0, 1, 2, 0])");
}

TEST_F(TestMatchKernel, MatchChunkedArrayInvoke) {
  std::vector<std::string> values1 = {"foo", "bar", "foo"};
  std::vector<std::string> values2 = {"bar", "baz", "quuux", "foo"};
  std::vector<std::string> values3 = {"foo", "bar", "foo"};
  std::vector<std::string> values4 = {"bar", "baz", "barr", "foo"};

  auto type = utf8();
  auto a1 = _MakeArray<StringType, std::string>(type, values1, {});
  auto a2 = _MakeArray<StringType, std::string>(type, values2, {true, true, true, false});
  auto a3 = _MakeArray<StringType, std::string>(type, values3, {});
  auto a4 = _MakeArray<StringType, std::string>(type, values4, {});

  ArrayVector array1 = {a1, a2};
  auto carr = std::make_shared<ChunkedArray>(array1);
  ArrayVector array2 = {a3, a4};
  auto member_set = std::make_shared<ChunkedArray>(array2);

  auto i1 = _MakeArray<Int32Type, int32_t>(int32(), {0, 1, 0}, {});
  auto i2 =
      _MakeArray<Int32Type, int32_t>(int32(), {1, 2, 2, 2}, {true, true, false, false});

  ArrayVector expected = {i1, i2};
  auto expected_carr = std::make_shared<ChunkedArray>(expected);

  Datum encoded_out;
  ASSERT_OK(Match(&this->ctx_, carr, member_set, &encoded_out));
  ASSERT_EQ(Datum::CHUNKED_ARRAY, encoded_out.kind());

  AssertChunkedEqual(*expected_carr, *encoded_out.chunked_array());
}

}  // namespace compute
}  // namespace arrow
