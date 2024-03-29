use std::marker::PhantomData;

use cosmwasm_std::{
  to_binary, to_vec, Addr, Api, Binary, ContractResult, Deps, Empty, Order, QuerierWrapper,
  QueryRequest, StdError, StdResult, Storage, SystemResult, Timestamp, WasmQuery,
};
use cw_storage_plus::{Bound, Map, PrefixBound};

use crate::{
  error::ContractError,
  models::{ContractID, IndexBounds},
  msg::{EntityContractEnvelope, ImplementorQueryMsg, Page, Since, Target},
  state::{
    get_bool_index, get_text_index, get_timestamp_index, get_u128_index, get_u64_index, ID_2_ADDR,
    IX_CODE_ID, IX_CREATED_AT, IX_CREATED_BY, IX_HEIGHT, IX_REV, IX_UPDATED_AT, METADATA,
    RELATIONSHIPS, TAGGED_CONTRACT_IDS,
  },
};

pub const MIN_LIMIT: u32 = 1;
pub const MAX_LIMIT: u32 = 50;
pub const DEFAULT_LIMIT: u32 = 25;

pub fn read(
  deps: Deps,
  target: &Target,
  maybe_desc: Option<bool>,
  maybe_limit: Option<u32>,
  maybe_fields: Option<Vec<String>>,
  maybe_since: Option<Since>,
  maybe_meta: Option<bool>,
  maybe_wallet: Option<Addr>,
  maybe_cursor: Option<(String, ContractID)>,
) -> Result<Page, ContractError> {
  // clamp limit to min and max bounds
  let limit = maybe_limit
    .unwrap_or(DEFAULT_LIMIT)
    .clamp(MIN_LIMIT, MAX_LIMIT);

  // resolve Order enum from desc flag
  let order = if maybe_desc.unwrap_or(false) {
    Order::Descending
  } else {
    Order::Ascending
  };

  // build vec of returned contract addresses from contract ID's, along with
  // any queried state from each contract, provided params is not None.
  let page: Page = match &target {
    Target::Index(bounds) => {
      let keys = read_index(deps, bounds, order, limit, maybe_cursor)?;
      build_contracts_page(
        deps,
        &keys,
        maybe_fields,
        maybe_since,
        maybe_meta,
        maybe_wallet,
      )
    },
    Target::Tag(tag) => {
      let keys = read_tags(deps, tag, order, limit, maybe_cursor)?;
      build_contracts_page(
        deps,
        &keys,
        maybe_fields,
        maybe_since,
        maybe_meta,
        maybe_wallet,
      )
    },
    Target::Relationship((rel_subject_addr, rel_name)) => {
      let keys = read_relationship(deps, rel_subject_addr, rel_name, order, limit, maybe_cursor)?;
      build_contracts_page(
        deps,
        &keys,
        maybe_fields,
        maybe_since,
        maybe_meta,
        maybe_wallet,
      )
    },
  }?;

  Ok(page)
}

fn build_contracts_page(
  deps: Deps,
  keys: &Vec<(String, ContractID)>,
  maybe_fields: Option<Vec<String>>,
  maybe_since: Option<Since>,
  maybe_meta: Option<bool>,
  maybe_wallet: Option<Addr>,
) -> Result<Page, ContractError> {
  let mut page_data: Vec<EntityContractEnvelope> = Vec::with_capacity(keys.len());
  let next_cursor: Option<(String, ContractID)> =
    keys.last().and_then(|x| Some(x.clone())).or(None);

  for (_, id) in keys.iter() {
    let contract_addr = ID_2_ADDR.load(deps.storage, *id)?;
    let some_meta = if maybe_meta.unwrap_or(false) {
      METADATA.may_load(deps.storage, contract_addr.clone())?
    } else {
      None
    };

    //skip if not modified since modified_since revision or timestamp
    if let Some(since) = maybe_since.clone() {
      let meta = if let Some(meta) = &some_meta {
        meta.clone()
      } else {
        METADATA.load(deps.storage, contract_addr.clone())?
      };
      match since {
        Since::Rev(rev) => {
          if meta.rev <= rev {
            continue;
          }
        },
        Since::Timestamp(time) => {
          if meta.updated_at <= time {
            continue;
          }
        },
      }
    }

    // query state from contract if fields vec is not None
    let state = if maybe_fields.is_some() {
      // an empty fields vec should be interpreted as "select *"
      Some(query_smart_no_deserialize(
        deps.api,
        deps.querier,
        &contract_addr,
        &maybe_fields,
        &maybe_wallet,
      )?)
    } else {
      None
    };

    page_data.push(EntityContractEnvelope {
      address: contract_addr.clone(),
      meta: some_meta,
      state,
    })
  }

  Ok(Page {
    page: page_data,
    cursor: next_cursor,
  })
}

fn read_tags(
  deps: Deps,
  tag: &String,
  order: Order,
  limit: u32,
  maybe_cursor: Option<(String, ContractID)>,
) -> Result<Vec<(String, ContractID)>, ContractError> {
  let map = TAGGED_CONTRACT_IDS;

  let iter = if let Some((tag, min_contract_id)) = maybe_cursor {
    let bound = Some(Bound::Exclusive((
      (tag.clone(), min_contract_id),
      PhantomData,
    )));
    match order {
      Order::Ascending => {
        let upper = Some(Bound::Inclusive((
          (tag.clone(), ContractID::MAX),
          PhantomData,
        )));
        map.range(deps.storage, bound, upper, order)
      },
      Order::Descending => {
        let lower = Some(Bound::Inclusive((
          (tag.clone(), ContractID::MIN),
          PhantomData,
        )));
        map.range(deps.storage, lower, bound, order)
      },
    }
  } else {
    map.prefix_range(
      deps.storage,
      Some(PrefixBound::Inclusive((tag.clone(), PhantomData))),
      Some(PrefixBound::Inclusive((tag.clone(), PhantomData))),
      order,
    )
  };

  collect(iter, limit, |k, _| Ok(k.clone()))
}

fn read_relationship(
  deps: Deps,
  rel_subject_addr: &Addr,
  rel_name: &String,
  order: Order,
  limit: u32,
  maybe_cursor: Option<(String, ContractID)>,
) -> Result<Vec<(String, ContractID)>, ContractError> {
  let map = RELATIONSHIPS;

  let iter = if let Some((name, min_contract_id)) = maybe_cursor {
    let bound = Some(Bound::Exclusive((
      (rel_subject_addr.clone(), name.clone(), min_contract_id),
      PhantomData,
    )));
    match order {
      Order::Ascending => {
        let upper = Some(Bound::Inclusive((
          (rel_subject_addr.clone(), name.clone(), ContractID::MAX),
          PhantomData,
        )));
        map.range(deps.storage, bound, upper, order)
      },
      Order::Descending => {
        let lower = Some(Bound::Inclusive((
          (rel_subject_addr.clone(), name.clone(), ContractID::MIN),
          PhantomData,
        )));
        map.range(deps.storage, lower, bound, order)
      },
    }
  } else {
    map.prefix_range(
      deps.storage,
      Some(PrefixBound::Inclusive((
        (rel_subject_addr.clone(), rel_name.clone()),
        PhantomData,
      ))),
      Some(PrefixBound::Inclusive((
        (rel_subject_addr.clone(), rel_name.clone()),
        PhantomData,
      ))),
      order,
    )
  };

  collect(iter, limit, |(_, name, id), _| Ok((name.clone(), id)))
}

fn read_index(
  deps: Deps,
  bounds: &IndexBounds,
  order: Order,
  limit: u32,
  maybe_cursor: Option<(String, ContractID)>,
) -> Result<Vec<(String, ContractID)>, ContractError> {
  let store = deps.storage;
  let api = deps.api;

  // compute vec of contract ID's from an index
  Ok(match bounds.clone() {
    IndexBounds::Address { equals, between } => {
      paginate_metadata(store, api, maybe_cursor, equals, between, order, limit)?
    },
    IndexBounds::CreatedBy { equals, between } => {
      let ix = &IX_CREATED_BY;
      paginate_addr_index(store, api, ix, equals, between, order, limit, maybe_cursor)?
    },
    IndexBounds::CreatedAt { equals, between } => {
      let ix = &IX_CREATED_AT;
      paginate_ts_index(store, ix, equals, between, order, limit, maybe_cursor)?
    },
    IndexBounds::UpdatedAt { equals, between } => {
      let ix = &IX_UPDATED_AT;
      paginate_ts_index(store, ix, equals, between, order, limit, maybe_cursor)?
    },
    IndexBounds::Uint64 {
      slot,
      between,
      equals,
    } => {
      let map = &get_u64_index(slot)?;
      paginate_u64_index(
        deps.storage,
        map,
        equals,
        between,
        order,
        limit,
        maybe_cursor,
      )?
    },
    IndexBounds::Text {
      slot,
      equals,
      between,
    } => {
      let map = &get_text_index(slot)?;
      paginate_str_index(
        deps.storage,
        map,
        equals,
        between,
        order,
        limit,
        maybe_cursor,
      )?
    },
    IndexBounds::Timestamp {
      slot,
      equals,
      between,
    } => {
      let ix = &get_timestamp_index(slot)?;
      paginate_ts_index(store, ix, equals, between, order, limit, maybe_cursor)?
    },
    IndexBounds::Rev { equals, between } => {
      let ix = &IX_REV;
      paginate_u64_index(store, ix, equals, between, order, limit, maybe_cursor)?
    },
    IndexBounds::CodeId { equals, between } => {
      let ix = &IX_CODE_ID;
      paginate_u64_index(store, ix, equals, between, order, limit, maybe_cursor)?
    },
    IndexBounds::Height { equals, between } => {
      let ix = &IX_HEIGHT;
      paginate_u64_index(store, ix, equals, between, order, limit, maybe_cursor)?
    },
    IndexBounds::Boolean { slot, start, stop } => {
      let map = get_bool_index(slot)?;
      paginate_bool_index(deps.storage, &map, start, stop, order, limit, maybe_cursor)?
    },
    IndexBounds::Uint128 {
      slot,
      between,
      equals,
    } => {
      let map = &get_u128_index(slot)?;
      paginate_u128_index(
        deps.storage,
        map,
        equals,
        between,
        order,
        limit,
        maybe_cursor,
      )?
    },
  })
}

fn query_smart_no_deserialize(
  api: &dyn Api,
  querier: QuerierWrapper<Empty>,
  contract_addr: &Addr,
  fields: &Option<Vec<String>>,
  wallet: &Option<Addr>,
) -> Result<Binary, ContractError> {
  let request: QueryRequest<Empty> = WasmQuery::Smart {
    contract_addr: contract_addr.clone().into(),
    msg: to_binary(&ImplementorQueryMsg::Select {
      wallet: wallet.clone(),
      fields: fields.clone(),
    })?,
  }
  .into();

  let raw = to_vec(&request).map_err(|serialize_err| {
    StdError::generic_err(format!("serializing QueryRequest: {}", serialize_err))
  })?;

  match querier.raw_query(&raw) {
    SystemResult::Ok(ContractResult::Ok(value)) => Ok(value),
    SystemResult::Err(system_err) => {
      let error_msg = format!(
        "contract error querying state of contract {}: {}",
        contract_addr, system_err
      );
      api.debug(error_msg.as_str());
      Err(ContractError::QueryStateError { msg: error_msg })
    },
    SystemResult::Ok(ContractResult::Err(contract_err)) => {
      let error_msg = format!(
        "contract error querying state of contract {}: {}",
        contract_addr, contract_err
      );
      api.debug(error_msg.as_str());
      Err(ContractError::QueryStateError { msg: error_msg })
    },
  }
}

fn paginate_metadata(
  store: &dyn Storage,
  api: &dyn Api,
  cursor: Option<(String, ContractID)>,
  equals: Option<Addr>,
  between: Option<(Option<Addr>, Option<Addr>)>,
  order: Order,
  limit: u32,
) -> Result<Vec<(String, ContractID)>, ContractError> {
  let map = METADATA;

  let (start, stop, is_exclusive) = if let Some(value) = equals {
    (Some(value.clone()), Some(value.clone()), false)
  } else if let Some((lower, upper)) = between {
    (lower, upper, true)
  } else {
    (None, None, true)
  };

  let cursor = cursor
    .and_then(|(x, _id)| api.addr_validate(x.as_str()).ok())
    .or(start);

  let start_bound = cursor
    .and_then(|x| Some(Bound::Inclusive((x, PhantomData))))
    .or(None);

  let stop_bound = stop
    .and_then(|x| {
      Some(if is_exclusive {
        Bound::Exclusive((x, PhantomData))
      } else {
        Bound::Inclusive((x, PhantomData))
      })
    })
    .or(None);

  let iter = match order {
    Order::Ascending => map.range(store, start_bound, stop_bound, order),
    Order::Descending => map.range(store, stop_bound, start_bound, order),
  };

  return collect(
    iter,
    limit,
    |k, v| -> Result<(String, ContractID), ContractError> { Ok((k.to_string(), v.id)) },
  );
}

fn paginate_u128_index<'a>(
  store: &dyn Storage,
  map: &Map<'a, (u128, ContractID), bool>,
  equals: Option<u128>,
  between: Option<(Option<u128>, Option<u128>)>,
  order: Order,
  limit: u32,
  cursor: Option<(String, ContractID)>,
) -> Result<Vec<(String, ContractID)>, ContractError> {
  let (start, stop, is_exclusive) = if let Some(value) = equals {
    (Some(value), Some(value), false)
  } else if let Some((lower, upper)) = between {
    (lower, upper, true)
  } else {
    (None, None, true)
  };

  let iter = if let Some((x, id)) = cursor {
    let start = match x.parse::<u128>() {
      Ok(x) => Some(Bound::Exclusive(((x, id), PhantomData))),
      Err(_) => None,
    };
    let stop = stop
      .and_then(|x| {
        Some(if is_exclusive {
          Bound::Exclusive(((x, 0), PhantomData))
        } else {
          Bound::Inclusive(((x, ContractID::MAX), PhantomData))
        })
      })
      .or(None);
    match order {
      Order::Ascending => map.range(store, start, stop, order),
      Order::Descending => map.range(store, stop, start, order),
    }
  } else {
    map.prefix_range(
      store,
      start
        .and_then(|n| Some(PrefixBound::Inclusive((n, PhantomData))))
        .or(None),
      stop
        .and_then(|n| {
          Some(if is_exclusive {
            PrefixBound::Exclusive((n, PhantomData))
          } else {
            PrefixBound::Inclusive((n, PhantomData))
          })
        })
        .or(None),
      order,
    )
  };

  return collect(
    iter,
    limit,
    |(x, id), _| -> Result<(String, ContractID), ContractError> { Ok((x.to_string(), id)) },
  );
}

fn paginate_u64_index<'a>(
  store: &dyn Storage,
  map: &Map<'a, (u64, ContractID), bool>,
  equals: Option<u64>,
  between: Option<(Option<u64>, Option<u64>)>,
  order: Order,
  limit: u32,
  cursor: Option<(String, ContractID)>,
) -> Result<Vec<(String, ContractID)>, ContractError> {
  let (start, stop, is_exclusive) = if let Some(value) = equals {
    (Some(value), Some(value), false)
  } else if let Some((lower, upper)) = between {
    (lower, upper, true)
  } else {
    (None, None, true)
  };

  let iter = if let Some((x, id)) = cursor {
    let start = match x.parse::<u64>() {
      Ok(x) => Some(Bound::Exclusive(((x, id), PhantomData))),
      Err(_) => None,
    };
    let stop = stop
      .and_then(|x| {
        Some(if is_exclusive {
          Bound::Exclusive(((x, 0), PhantomData))
        } else {
          Bound::Inclusive(((x, ContractID::MAX), PhantomData))
        })
      })
      .or(None);
    match order {
      Order::Ascending => map.range(store, start, stop, order),
      Order::Descending => map.range(store, stop, start, order),
    }
  } else {
    map.prefix_range(
      store,
      start
        .and_then(|n| Some(PrefixBound::Inclusive((n, PhantomData))))
        .or(None),
      stop
        .and_then(|n| {
          Some(if is_exclusive {
            PrefixBound::Exclusive((n, PhantomData))
          } else {
            PrefixBound::Inclusive((n, PhantomData))
          })
        })
        .or(None),
      order,
    )
  };

  return collect(
    iter,
    limit,
    |(x, id), _| -> Result<(String, ContractID), ContractError> { Ok((x.to_string(), id)) },
  );
}

fn paginate_bool_index<'a>(
  store: &dyn Storage,
  map: &Map<'a, (u8, ContractID), bool>,
  start: Option<bool>,
  stop: Option<bool>,
  order: Order,
  limit: u32,
  raw_cursor: Option<(String, ContractID)>,
) -> Result<Vec<(String, ContractID)>, ContractError> {
  let iter = if let Some((x, id)) = raw_cursor {
    let bool_binary = if !(x.to_lowercase() == "false" || x == "0") {
      1u8
    } else {
      0u8
    };
    let start = Some(Bound::Exclusive(((bool_binary, id), PhantomData)));
    let stop = stop
      .and_then(|x| Some(Bound::Exclusive(((if x { 1 } else { 0 }, 0), PhantomData))))
      .or(None);
    match order {
      Order::Ascending => map.range(store, start, stop, order),
      Order::Descending => map.range(store, stop, start, order),
    }
  } else {
    map.prefix_range(
      store,
      start
        .and_then(|x| Some(PrefixBound::Inclusive((if x { 1 } else { 0 }, PhantomData))))
        .or(None),
      stop
        .and_then(|x| Some(PrefixBound::Exclusive((if x { 1 } else { 0 }, PhantomData))))
        .or(None),
      order,
    )
  };

  return collect(
    iter,
    limit,
    |(x, id), _| -> Result<(String, ContractID), ContractError> { Ok((x.to_string(), id)) },
  );
}

fn paginate_str_index<'a>(
  store: &dyn Storage,
  map: &Map<'a, (String, ContractID), bool>,
  equals: Option<String>,
  between: Option<(Option<String>, Option<String>)>,
  order: Order,
  limit: u32,
  cursor: Option<(String, ContractID)>,
) -> Result<Vec<(String, ContractID)>, ContractError> {
  let (start, stop, is_exclusive) = if let Some(value) = equals {
    (Some(value.clone()), Some(value.clone()), false)
  } else if let Some((lower, upper)) = between {
    (lower, upper, true)
  } else {
    (None, None, true)
  };

  let iter = if let Some(cur) = cursor {
    let start = Some(Bound::Exclusive((cur, PhantomData)));
    let stop = stop
      .and_then(|x| {
        Some(if is_exclusive {
          Bound::Exclusive(((x, 0), PhantomData))
        } else {
          Bound::Inclusive(((x, 0), PhantomData))
        })
      })
      .or(None);
    match order {
      Order::Ascending => map.range(store, start, stop, order),
      Order::Descending => map.range(store, stop, start, order),
    }
  } else {
    map.prefix_range(
      store,
      start
        .and_then(|n| Some(PrefixBound::Inclusive((n, PhantomData))))
        .or(None),
      stop
        .and_then(|n| {
          Some(if is_exclusive {
            PrefixBound::Exclusive((n, PhantomData))
          } else {
            PrefixBound::Inclusive((n, PhantomData))
          })
        })
        .or(None),
      order,
    )
  };

  return collect(
    iter,
    limit,
    |k, _| -> Result<(String, ContractID), ContractError> { Ok(k) },
  );
}

fn paginate_addr_index<'a>(
  store: &dyn Storage,
  api: &dyn Api,
  map: &Map<'a, (Addr, ContractID), bool>,
  equals: Option<Addr>,
  between: Option<(Option<Addr>, Option<Addr>)>,
  order: Order,
  limit: u32,
  cursor: Option<(String, ContractID)>,
) -> Result<Vec<(String, ContractID)>, ContractError> {
  let (start, stop, is_exclusive) = if let Some(value) = equals {
    (Some(value.clone()), Some(value.clone()), false)
  } else if let Some((lower, upper)) = between {
    (lower, upper, true)
  } else {
    (None, None, true)
  };

  let iter = if let Some((x, id)) = cursor {
    let start = if let Ok(addr) = api.addr_validate(x.as_str()) {
      Some(Bound::Exclusive(((addr, id), PhantomData)))
    } else {
      None
    };
    let stop = stop
      .and_then(|addr| {
        Some(if is_exclusive {
          Bound::Exclusive(((addr, 0), PhantomData))
        } else {
          Bound::Inclusive(((addr, 0), PhantomData))
        })
      })
      .or(None);
    match order {
      Order::Ascending => map.range(store, start, stop, order),
      Order::Descending => map.range(store, stop, start, order),
    }
  } else {
    map.prefix_range(
      store,
      start
        .and_then(|n| Some(PrefixBound::Inclusive((n, PhantomData))))
        .or(None),
      stop
        .and_then(|n| {
          Some(if is_exclusive {
            PrefixBound::Exclusive((n, PhantomData))
          } else {
            PrefixBound::Inclusive((n, PhantomData))
          })
        })
        .or(None),
      order,
    )
  };

  return collect(
    iter,
    limit,
    |(addr, id), _| -> Result<(String, ContractID), ContractError> { Ok((addr.to_string(), id)) },
  );
}

fn paginate_ts_index<'a>(
  store: &dyn Storage,
  map: &Map<'a, (u64, ContractID), bool>,
  equals: Option<Timestamp>,
  between: Option<(Option<Timestamp>, Option<Timestamp>)>,
  order: Order,
  limit: u32,
  raw_cursor: Option<(String, ContractID)>,
) -> Result<Vec<(String, ContractID)>, ContractError> {
  paginate_u64_index(
    store,
    map,
    equals.and_then(|x| Some(x.nanos())).or(None),
    between
      .and_then(|(l, u)| {
        Some((
          l.and_then(|t| Some(t.nanos())).or(None),
          u.and_then(|t| Some(t.nanos())).or(None),
        ))
      })
      .or(None),
    order,
    limit,
    raw_cursor,
  )
}

pub fn collect<'a, D, T, R, E, F>(
  iter: Box<dyn Iterator<Item = StdResult<(D, T)>> + 'a>,
  limit: u32,
  parse_fn: F,
) -> Result<Vec<R>, E>
where
  F: Fn(D, T) -> Result<R, E>,
  E: From<StdError>,
{
  let limit = limit as usize;
  iter
    .take(limit)
    .map(|item| {
      let (k, v) = item?;
      parse_fn(k, v)
    })
    .collect()
}
