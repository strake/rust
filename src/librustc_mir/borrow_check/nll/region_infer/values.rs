// Copyright 2017 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use rustc::mir::{BasicBlock, Location, Mir};
use rustc::ty::RegionVid;
use rustc_data_structures::bitvec::SparseBitMatrix;
use rustc_data_structures::indexed_vec::Idx;
use rustc_data_structures::indexed_vec::IndexVec;
use std::fmt::Debug;
use std::rc::Rc;

/// Maps between the various kinds of elements of a region value to
/// the internal indices that w use.
crate struct RegionValueElements {
    /// For each basic block, how many points are contained within?
    statements_before_block: IndexVec<BasicBlock, usize>,
    num_points: usize,
    num_universal_regions: usize,
}

impl RegionValueElements {
    crate fn new(mir: &Mir<'_>, num_universal_regions: usize) -> Self {
        let mut num_points = 0;
        let statements_before_block = mir
            .basic_blocks()
            .iter()
            .map(|block_data| {
                let v = num_points;
                num_points += block_data.statements.len() + 1;
                v
            })
            .collect();

        debug!(
            "RegionValueElements(num_universal_regions={:?})",
            num_universal_regions
        );
        debug!(
            "RegionValueElements: statements_before_block={:#?}",
            statements_before_block
        );
        debug!("RegionValueElements: num_points={:#?}", num_points);

        Self {
            statements_before_block,
            num_universal_regions,
            num_points,
        }
    }

    /// Total number of element indices that exist.
    crate fn num_elements(&self) -> usize {
        self.num_points + self.num_universal_regions
    }

    /// Converts an element of a region value into a `RegionElementIndex`.
    crate fn index<T: ToElementIndex>(&self, elem: T) -> RegionElementIndex {
        elem.to_element_index(self)
    }

    /// Iterates over the `RegionElementIndex` for all points in the CFG.
    crate fn all_point_indices<'a>(&'a self) -> impl Iterator<Item = RegionElementIndex> + 'a {
        (0..self.num_points).map(move |i| RegionElementIndex::new(i + self.num_universal_regions))
    }

    /// Converts a particular `RegionElementIndex` to the `RegionElement` it represents.
    crate fn to_element(&self, i: RegionElementIndex) -> RegionElement {
        debug!("to_element(i={:?})", i);

        if let Some(r) = self.to_universal_region(i) {
            RegionElement::UniversalRegion(r)
        } else {
            let point_index = i.index() - self.num_universal_regions;

            // Find the basic block. We have a vector with the
            // starting index of the statement in each block. Imagine
            // we have statement #22, and we have a vector like:
            //
            // [0, 10, 20]
            //
            // In that case, this represents point_index 2 of
            // basic block BB2. We know this because BB0 accounts for
            // 0..10, BB1 accounts for 11..20, and BB2 accounts for
            // 20...
            //
            // To compute this, we could do a binary search, but
            // because I am lazy we instead iterate through to find
            // the last point where the "first index" (0, 10, or 20)
            // was less than the statement index (22). In our case, this will
            // be (BB2, 20).
            //
            // Nit: we could do a binary search here but I'm too lazy.
            let (block, &first_index) = self
                .statements_before_block
                .iter_enumerated()
                .filter(|(_, first_index)| **first_index <= point_index)
                .last()
                .unwrap();

            RegionElement::Location(Location {
                block,
                statement_index: point_index - first_index,
            })
        }
    }

    /// Converts a particular `RegionElementIndex` to a universal
    /// region, if that is what it represents. Returns `None`
    /// otherwise.
    crate fn to_universal_region(&self, i: RegionElementIndex) -> Option<RegionVid> {
        if i.index() < self.num_universal_regions {
            Some(RegionVid::new(i.index()))
        } else {
            None
        }
    }
}

/// A newtype for the integers that represent one of the possible
/// elements in a region. These are the rows in the `SparseBitMatrix` that
/// is used to store the values of all regions. They have the following
/// convention:
///
/// - The first N indices represent free regions (where N = universal_regions.len()).
/// - The remainder represent the points in the CFG (see `point_indices` map).
///
/// You can convert a `RegionElementIndex` into a `RegionElement`
/// using the `to_region_elem` method.
newtype_index!(RegionElementIndex { DEBUG_FORMAT = "RegionElementIndex({})" });

/// An individual element in a region value -- the value of a
/// particular region variable consists of a set of these elements.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
crate enum RegionElement {
    /// A point in the control-flow graph.
    Location(Location),

    /// An in-scope, universally quantified region (e.g., a lifetime parameter).
    UniversalRegion(RegionVid),
}

crate trait ToElementIndex: Debug + Copy {
    fn to_element_index(self, elements: &RegionValueElements) -> RegionElementIndex;
}

impl ToElementIndex for Location {
    fn to_element_index(self, elements: &RegionValueElements) -> RegionElementIndex {
        let Location {
            block,
            statement_index,
        } = self;
        let start_index = elements.statements_before_block[block];
        RegionElementIndex::new(elements.num_universal_regions + start_index + statement_index)
    }
}

impl ToElementIndex for RegionVid {
    fn to_element_index(self, elements: &RegionValueElements) -> RegionElementIndex {
        assert!(self.index() < elements.num_universal_regions);
        RegionElementIndex::new(self.index())
    }
}

impl ToElementIndex for RegionElementIndex {
    fn to_element_index(self, _elements: &RegionValueElements) -> RegionElementIndex {
        self
    }
}

/// Stores the values for a set of regions. These are stored in a
/// compact `SparseBitMatrix` representation, with one row per region
/// variable. The columns consist of either universal regions or
/// points in the CFG.
#[derive(Clone)]
crate struct RegionValues<N: Idx> {
    elements: Rc<RegionValueElements>,
    matrix: SparseBitMatrix<N, RegionElementIndex>,
}

impl<N: Idx> RegionValues<N> {
    /// Creates a new set of "region values" that tracks causal information.
    /// Each of the regions in num_region_variables will be initialized with an
    /// empty set of points and no causal information.
    crate fn new(elements: &Rc<RegionValueElements>, num_region_variables: usize) -> Self {
        assert!(
            elements.num_universal_regions <= num_region_variables,
            "universal regions are a subset of the region variables"
        );

        Self {
            elements: elements.clone(),
            matrix: SparseBitMatrix::new(
                N::new(num_region_variables),
                RegionElementIndex::new(elements.num_elements()),
            ),
        }
    }

    /// Adds the given element to the value for the given region. Returns true if
    /// the element is newly added (i.e., was not already present).
    crate fn add_element(
        &mut self,
        r: N,
        elem: impl ToElementIndex,
    ) -> bool {
        let i = self.elements.index(elem);
        debug!("add(r={:?}, elem={:?})", r, elem);
        self.matrix.add(r, i)
    }

    /// Add all elements in `r_from` to `r_to` (because e.g. `r_to:
    /// r_from`).
    crate fn add_region(&mut self, r_to: N, r_from: N) -> bool {
        self.matrix.merge(r_from, r_to)
    }

    /// True if the region `r` contains the given element.
    crate fn contains(&self, r: N, elem: impl ToElementIndex) -> bool {
        let i = self.elements.index(elem);
        self.matrix.contains(r, i)
    }

    /// True if `sup_region` contains all the CFG points that
    /// `sub_region` contains. Ignores universal regions.
    crate fn contains_points(&self, sup_region: N, sub_region: N) -> bool {
        // This could be done faster by comparing the bitsets. But I
        // am lazy.
        self.element_indices_contained_in(sub_region)
            .skip_while(|&i| self.elements.to_universal_region(i).is_some())
            .all(|e| self.contains(sup_region, e))
    }

    /// Iterate over the value of the region `r`, yielding up element
    /// indices. You may prefer `universal_regions_outlived_by` or
    /// `elements_contained_in`.
    crate fn element_indices_contained_in<'a>(
        &'a self,
        r: N,
    ) -> impl Iterator<Item = RegionElementIndex> + 'a {
        self.matrix.iter(r).map(move |i| i)
    }

    /// Returns just the universal regions that are contained in a given region's value.
    crate fn universal_regions_outlived_by<'a>(
        &'a self,
        r: N,
    ) -> impl Iterator<Item = RegionVid> + 'a {
        self.element_indices_contained_in(r)
            .map(move |i| self.elements.to_universal_region(i))
            .take_while(move |v| v.is_some()) // universal regions are a prefix
            .map(move |v| v.unwrap())
    }

    /// Returns all the elements contained in a given region's value.
    crate fn elements_contained_in<'a>(
        &'a self,
        r: N,
    ) -> impl Iterator<Item = RegionElement> + 'a {
        self.element_indices_contained_in(r)
            .map(move |r| self.elements.to_element(r))
    }

    /// Returns a "pretty" string value of the region. Meant for debugging.
    crate fn region_value_str(&self, r: N) -> String {
        let mut result = String::new();
        result.push_str("{");

        // Set to Some(l1, l2) when we have observed all the locations
        // from l1..=l2 (inclusive) but not yet printed them. This
        // gets extended if we then see l3 where l3 is the successor
        // to l2.
        let mut open_location: Option<(Location, Location)> = None;

        let mut sep = "";
        let mut push_sep = |s: &mut String| {
            s.push_str(sep);
            sep = ", ";
        };

        for element in self.elements_contained_in(r) {
            match element {
                RegionElement::Location(l) => {
                    if let Some((location1, location2)) = open_location {
                        if location2.block == l.block
                            && location2.statement_index == l.statement_index - 1
                        {
                            open_location = Some((location1, l));
                            continue;
                        }

                        push_sep(&mut result);
                        Self::push_location_range(&mut result, location1, location2);
                    }

                    open_location = Some((l, l));
                }

                RegionElement::UniversalRegion(fr) => {
                    if let Some((location1, location2)) = open_location {
                        push_sep(&mut result);
                        Self::push_location_range(&mut result, location1, location2);
                        open_location = None;
                    }

                    push_sep(&mut result);
                    result.push_str(&format!("{:?}", fr));
                }
            }
        }

        if let Some((location1, location2)) = open_location {
            push_sep(&mut result);
            Self::push_location_range(&mut result, location1, location2);
        }

        result.push_str("}");

        result
    }

    fn push_location_range(str: &mut String, location1: Location, location2: Location) {
        if location1 == location2 {
            str.push_str(&format!("{:?}", location1));
        } else {
            assert_eq!(location1.block, location2.block);
            str.push_str(&format!(
                "{:?}[{}..={}]",
                location1.block, location1.statement_index, location2.statement_index
            ));
        }
    }
}
