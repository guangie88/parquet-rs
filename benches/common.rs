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

extern crate rand;
extern crate parquet;

use std::rc::Rc;
use rand::{thread_rng, Rng};

use parquet::basic::*;
use parquet::data_type::*;
use parquet::schema::types::{Type as SchemaType, ColumnDescriptor, ColumnPath};

macro_rules! gen_random_ints {
  ($fname:ident, $limit:expr) => {
    pub fn $fname(total: usize) -> Vec<i32> {
      let mut values = Vec::with_capacity(total);
      let mut rng = thread_rng();
      for _ in 0..total {
        values.push(rng.gen_range::<i32>(0, $limit));
      }
      values
    }
  }
}

gen_random_ints!(gen_10, 10);
gen_random_ints!(gen_100, 100);
gen_random_ints!(gen_1000, 1000);

pub fn gen_test_strs(total: usize) -> Vec<ByteArray> {
  let mut words = Vec::new();
  words.push("aaaaaaaaaa");
  words.push("bbbbbbbbbb");
  words.push("cccccccccc");
  words.push("dddddddddd");
  words.push("eeeeeeeeee");
  words.push("ffffffffff");
  words.push("gggggggggg");
  words.push("hhhhhhhhhh");
  words.push("iiiiiiiiii");
  words.push("jjjjjjjjjj");

  let mut rnd = rand::thread_rng();
  let mut values = Vec::new();
  for _ in 0..total {
    let idx = rnd.gen_range::<usize>(0, 10);
    values.push(ByteArray::from(words[idx]));
  }
  values
}

pub fn col_desc(type_length: i32, primitive_ty: Type) -> ColumnDescriptor {
  let ty = SchemaType::primitive_type_builder("col", primitive_ty)
    .with_length(type_length)
    .build()
    .unwrap();
  ColumnDescriptor::new(Rc::new(ty), None, 0, 0, ColumnPath::new(vec!()))
}
