use std::collections::VecDeque;

use anoma_vm_env::matchmaker_prelude::intent::{
    DecimalWrapper, Exchange, FungibleTokenIntent, IntentTransfers,
};
use anoma_vm_env::matchmaker_prelude::key::ed25519::Signed;
use anoma_vm_env::matchmaker_prelude::token::Amount;
use anoma_vm_env::matchmaker_prelude::*;
use good_lp::{default_solver, variable, variables, Solution, SolverModel};
use petgraph::graph::{node_index, DiGraph, NodeIndex};
use petgraph::visit::{depth_first_search, Control, DfsEvent};
use petgraph::Graph;
use rust_decimal::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ExchangeNode {
    id: Vec<u8>,
    exchange: Signed<Exchange>,
    intent: Signed<FungibleTokenIntent>,
}

#[matchmaker]
fn add_intent(graph_bytes: Vec<u8>, id: Vec<u8>, data: Vec<u8>) -> bool {
    let intent = decode_intent_data(&data);
    let exchanges = intent.data.exchange.clone();
    let mut graph = decode_graph(graph_bytes);
    log_string(format!("trying to match intent: {:#?}", intent));
    exchanges.into_iter().for_each(|exchange| {
        add_node(&mut graph, id.clone(), exchange, intent.clone())
    });
    find_match_and_remove_node(&mut graph);
    update_graph_data(&graph);
    true
}

fn create_transfer(
    from_node: &ExchangeNode,
    to_node: &ExchangeNode,
) -> token::Transfer {
    // we want the minimum between
    // - 1 / FROM_NODE.rate_min * TO_NODE.max_sell
    // -
    let max_amount = (Decimal::from_i128(1).unwrap()
        / from_node.exchange.data.rate_min.0)
        * Decimal::from_i128(to_node.exchange.data.max_sell.change()).unwrap();
    let amount = std::cmp::min(
        from_node.exchange.data.max_sell.change(),
        max_amount.to_i128().unwrap(),
    );

    token::Transfer {
        source: from_node.exchange.data.addr.clone(),
        target: to_node.exchange.data.addr.clone(),
        token: to_node.exchange.data.token_buy.clone(),
        amount: Amount::from(amount as u64),
    }
}

fn send_tx(tx_data: IntentTransfers) {
    let tx_data_bytes = tx_data.try_to_vec().unwrap();
    send_match(tx_data_bytes);
}

fn decode_intent_data(bytes: &[u8]) -> Signed<FungibleTokenIntent> {
    Signed::<FungibleTokenIntent>::try_from_slice(bytes).unwrap()
}

fn decode_graph(bytes: Vec<u8>) -> DiGraph<ExchangeNode, Address> {
    if bytes.is_empty() {
        Graph::new()
    } else {
        serde_json::from_slice(&bytes[..]).expect("error in json format")
    }
}

fn update_graph_data(graph: &DiGraph<ExchangeNode, Address>) {
    update_data(serde_json::to_vec(graph).unwrap());
}

fn find_to_update_node(
    graph: &DiGraph<ExchangeNode, Address>,
    new_node: &ExchangeNode,
) -> (Vec<NodeIndex>, Vec<NodeIndex>) {
    let start = node_index(0);
    let mut connect_sell = Vec::new();
    let mut connect_buy = Vec::new();
    depth_first_search(graph, Some(start), |event| {
        if let DfsEvent::Discover(index, _time) = event {
            let current_node = &graph[index];
            if new_node.exchange.data.token_sell
                == current_node.exchange.data.token_buy
            {
                connect_sell.push(index);
            }
            if new_node.exchange.data.token_buy
                == current_node.exchange.data.token_sell
            {
                connect_buy.push(index);
            }

            // let inverse_rate: Decimal =
            //     Decimal::from(1) / new_node.exchange.data.rate_min.0;
            // let current_node = &graph[index];
            // if new_node.exchange.data.token_sell
            //     == current_node.exchange.data.token_buy
            //     && new_node.exchange.data.max_sell
            //         >= current_node.exchange.data.min_buy
            //     && inverse_rate >= current_node.exchange.data.rate_min.0
            // {
            //     connect_sell.push(index);
            // } else if new_node.exchange.data.token_buy
            //     == current_node.exchange.data.token_sell
            //     && new_node.exchange.data.min_buy
            //         <= current_node.exchange.data.max_sell
            //     && inverse_rate <= current_node.exchange.data.rate_min.0
            // {
            //     connect_buy.push(index);
            // }
        }
        Control::<()>::Continue
    });
    (connect_sell, connect_buy)
}

fn add_node(
    graph: &mut DiGraph<ExchangeNode, Address>,
    id: Vec<u8>,
    exchange: Signed<Exchange>,
    intent: Signed<FungibleTokenIntent>,
) {
    let new_node = ExchangeNode {
        id,
        exchange,
        intent,
    };
    let new_node_index = graph.add_node(new_node.clone());
    log_string(format!("before graph: {:?}", graph));
    let (connect_sell, connect_buy) = find_to_update_node(&graph, &new_node);
    let sell_edge = new_node.exchange.data.token_sell;
    let buy_edge = new_node.exchange.data.token_buy;
    for node_index in connect_sell {
        graph.update_edge(new_node_index, node_index, sell_edge.clone());
    }
    for node_index in connect_buy {
        graph.update_edge(node_index, new_node_index, buy_edge.clone());
    }
    log_string(format!("after graph: {:?}", graph));
}

fn create_and_send_tx_data(
    graph: &DiGraph<ExchangeNode, Address>,
    cycle_intents: Vec<NodeIndex>,
) {
    log_string(format!(
        "found match; creating tx with {:?} nodes",
        cycle_intents.len()
    ));
    let cycle_intents = sort_cycle(graph, cycle_intents);
    let amounts = compute_amounts(graph, &cycle_intents);
    let mut cycle_intents_iter = cycle_intents.into_iter();
    let first_node = cycle_intents_iter.next().map(|i| &graph[i]).unwrap();
    let mut tx_data = IntentTransfers::empty();
    let last_node =
        cycle_intents_iter.fold(first_node, |prev_node, intent_index| {
            let node = &graph[intent_index];
            tx_data.transfers.insert(create_transfer(node, prev_node));
            tx_data
                .exchanges
                .insert(node.exchange.data.addr.clone(), node.exchange.clone());
            tx_data
                .intents
                .insert(node.exchange.data.addr.clone(), node.intent.clone());
            &node
        });
    tx_data
        .transfers
        .insert(create_transfer(first_node, last_node));
    tx_data.exchanges.insert(
        first_node.exchange.data.addr.clone(),
        first_node.exchange.clone(),
    );
    tx_data.intents.insert(
        first_node.exchange.data.addr.clone(),
        first_node.intent.clone(),
    );
    log_string(format!("tx data: {:?}", tx_data.transfers));
    send_tx(tx_data)
}

fn compute_amounts(
    graph: &DiGraph<ExchangeNode, Address>,
    cycle_intents: &Vec<NodeIndex>,
) -> Vec<Decimal> {
    let intent_graph = graph.filter_map(
        |node_index, node| {
            if cycle_intents.contains(&node_index) {
                Some(node)
            } else {
                None
            }
        },
        |_edge_index, edge| Some(edge),
    );
    log_string(format!("{:?}", intent_graph));

    let mut vars = variables!();

    depth_first_search(intent_graph, Some(start), |event| {
        if let DfsEvent::Discover(index, _time) = event {
            let current_node = &graph[index];
            // get next node following th edge
            let edges = currect_node.edges();
            &current_node
        }
        Control::<()>::Continue
    });

    vars.add(variable().min(2).max(9));

    Vec::new()
}

// The cycle returned by tarjan_scc only contains the node_index in an arbitrary
// order without edges. we must reorder them to craft the transfer

fn sort_cycle(
    graph: &DiGraph<ExchangeNode, Address>,
    cycle_intents: Vec<NodeIndex>,
) -> Vec<NodeIndex> {
    let mut cycle_ordered = Vec::new();
    let mut cycle_intents = VecDeque::from(cycle_intents);
    let mut to_connect_node = cycle_intents.pop_front().unwrap();
    cycle_ordered.push(to_connect_node);
    while !cycle_intents.is_empty() {
        let pop_node = cycle_intents.pop_front().unwrap();
        if graph.contains_edge(to_connect_node, pop_node) {
            cycle_ordered.push(pop_node);
            to_connect_node = pop_node;
        } else {
            cycle_intents.push_back(pop_node);
        }
    }
    cycle_ordered.reverse();
    cycle_ordered
}

fn find_match_and_send_tx(
    graph: &DiGraph<ExchangeNode, Address>,
) -> Vec<NodeIndex> {
    let mut to_remove_nodes = Vec::new();
    for cycle_intents in petgraph::algo::tarjan_scc(&graph) {
        // a node is a cycle with itself
        if cycle_intents.len() > 1 {
            to_remove_nodes.extend(&cycle_intents);
            create_and_send_tx_data(graph, cycle_intents);
        }
    }
    to_remove_nodes
}

fn find_match_and_remove_node(graph: &mut DiGraph<ExchangeNode, Address>) {
    let mut to_remove_nodes = find_match_and_send_tx(&graph);
    // Must be sorted in reverse order because it removes the node by index
    // otherwise it would not remove the correct node
    to_remove_nodes.sort_by(|a, b| b.cmp(a));
    to_remove_nodes.into_iter().for_each(|i| {
        graph.remove_node(i);
    });
}
