# Simulating p2p address relay

We want to design a simulation to try out different address relay timestamping techniques. Our goal is to find out how the network would react to different timestamping techniques.

## General overview

We are interested in seeing if we can safely change the GETADDR response algorithm to diminish the surface of timestamping.

Currently nodes send a GETADDR message to their peers when connecting. GETADDR are sent only if the peer is outbound and if the connection is not block only. Nodes respond by sending an ADDR message containing 1000 (address, timestamp) pairs from the node addrman. To avoid being able to scrape one node's entire addrman, GETADDR responses are cached for ~24 hours. 

There is a fingerprint attack on the GETADDR response, by looking at the timestamp paired with each address. When two nodes reliably return the same timestamp for each address in the response, there is a high chance that those two nodes are the same one.

We are studying different algorithms to mitigate the fingerprint attack, while being very mindful of not introducing problems in the network. This simulation is designed to study the effects of each new algorithm.

### Timestamping algorithms

The simulation includes three different algorithms:
Current algorithm - can be used to test that everything is working ok:
- Send right timestamp, as seen in addrman, cached every ~24 hours

Algorithm one - "Fixed in the past" - can be used to test that nodes that are not in the network anymore remain longer than expected (see https://github.com/bitcoin/bitcoin/pull/33498#pullrequestreview-3319680730):
- Send 5 days ago

Algorithm two - "Based on network" - our best candidate, for now:
- Same network as me: send right timestamp
- Different network: send 5 days ago

## Inputs
Simulation inputs:
- no of onion nodes
- no of clearnet nodes
- no of onion+clearnet
- % of nodes accepting incoming? or something to manipulate no. of connections
- how many new nodes join the network each day?
- how many new nodes leave the network each day?
- number of days of simulation

## Bitcoin Core specific behavior
Messages to handle:
- GETADDR req when connecting to outbound peer
- GETADDR resp when req received
- self announce once every 24 hours
- relay self announcements

Timestamps are updated when:
- New connection
- New announcement
- See fresher timestamp in getaddr

Important: timestamps are NOT updated when feelers connections succeed, so we don't need to care about feelers at all.

## What are we trying to see? 

Do addresses stick around longer than needed? See https://github.com/bitcoin/bitcoin/pull/33498#pullrequestreview-3319680730

With the new method, is it still possible to fingerprint? We can double check that the simulation is working correctly by testing the fingerprint in the current algorithm.

