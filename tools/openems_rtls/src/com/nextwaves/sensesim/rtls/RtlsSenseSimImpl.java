package com.nextwaves.sensesim.rtls;

import java.util.Map;

import org.osgi.service.component.ComponentContext;
import org.osgi.service.event.Event;
import org.osgi.service.event.EventHandler;

import io.openems.common.channel.AccessMode;
import io.openems.common.types.OpenemsType;
import io.openems.edge.common.channel.Doc;
import io.openems.edge.common.channel.IntegerWriteChannel;
import io.openems.edge.common.component.AbstractOpenemsComponent;
import io.openems.edge.common.component.OpenemsComponent;

/**
 * Sense Sim RTLS values inside OpenEMS Edge.
 *
 * <p>
 * Every channel is a writable integer. Sense Sim writes the live metrics
 * through Controller.Api.Rest.ReadWrite (POST /rest/channel/rtls0/TagsLocated
 * {"value": 5}) and the value is published on the next cycle, so OpenEMS
 * controllers, the UI, timedata and the backend see it like any other
 * channel. SimRunning / SimSpeedX10 go the other way: OpenEMS writes them and
 * Sense Sim reads them back to pause or speed up the simulation.
 */
public class RtlsSenseSimImpl extends AbstractOpenemsComponent implements OpenemsComponent, EventHandler {

	private static final ChannelId[] ZONES = { ChannelId.ZONE1_OCCUPANCY, ChannelId.ZONE2_OCCUPANCY, ChannelId.ZONE3_OCCUPANCY,
			ChannelId.ZONE4_OCCUPANCY, ChannelId.ZONE5_OCCUPANCY, ChannelId.ZONE6_OCCUPANCY, ChannelId.ZONE7_OCCUPANCY,
			ChannelId.ZONE8_OCCUPANCY };
	private static final ChannelId[] ACTUATORS = { ChannelId.ACTUATOR1, ChannelId.ACTUATOR2, ChannelId.ACTUATOR3,
			ChannelId.ACTUATOR4, ChannelId.ACTUATOR5, ChannelId.ACTUATOR6, ChannelId.ACTUATOR7, ChannelId.ACTUATOR8 };

	public enum ChannelId implements io.openems.edge.common.channel.ChannelId {
		TAGS_TOTAL, //
		TAGS_LOCATED, //
		TAGS_LOST, //
		INFRA_ONLINE, //
		INFRA_TOTAL, //
		ALARMS_ACTIVE, //
		RMSE_CM, //
		CEP95_CM, //
		RFID_READS_PER_SEC, //
		ZONE1_OCCUPANCY, //
		ZONE2_OCCUPANCY, //
		ZONE3_OCCUPANCY, //
		ZONE4_OCCUPANCY, //
		ZONE5_OCCUPANCY, //
		ZONE6_OCCUPANCY, //
		ZONE7_OCCUPANCY, //
		ZONE8_OCCUPANCY, //
		/*
		 * RF analysis over the radio links of the last update (all technologies):
		 * links attempted, % heard, average / weakest signal (dBm), weak links,
		 * % NLOS (path crosses objects or walls), links lost behind obstructions,
		 * mean UWB range bias on NLOS links (cm), tags heard by fewer than 3
		 * anchors, anchors that are weak or hear nothing, and from the last
		 * coverage scan the % of floor with a fix and the dead area (m2, -1 =
		 * not scanned).
		 */
		RF_LINKS, //
		RF_SUCCESS_PCT, //
		RF_AVG_DBM, //
		RF_MIN_DBM, //
		RF_WEAK_LINKS, //
		RF_NLOS_PCT, //
		RF_BLOCKED_LINKS, //
		RF_NLOS_BIAS_CM, //
		RF_TAGS_UNDER3, //
		RF_ANCHORS_WEAK, //
		RF_COVERED_PCT, //
		RF_DEAD_AREA_M2, //
		/* Health of anchors / beacons / readers 1..8 (sorted by id). */
		ANCHOR1_HEARD, ANCHOR1_SUCCESS_PCT, ANCHOR1_DBM, ANCHOR1_NLOS_PCT, //
		ANCHOR2_HEARD, ANCHOR2_SUCCESS_PCT, ANCHOR2_DBM, ANCHOR2_NLOS_PCT, //
		ANCHOR3_HEARD, ANCHOR3_SUCCESS_PCT, ANCHOR3_DBM, ANCHOR3_NLOS_PCT, //
		ANCHOR4_HEARD, ANCHOR4_SUCCESS_PCT, ANCHOR4_DBM, ANCHOR4_NLOS_PCT, //
		ANCHOR5_HEARD, ANCHOR5_SUCCESS_PCT, ANCHOR5_DBM, ANCHOR5_NLOS_PCT, //
		ANCHOR6_HEARD, ANCHOR6_SUCCESS_PCT, ANCHOR6_DBM, ANCHOR6_NLOS_PCT, //
		ANCHOR7_HEARD, ANCHOR7_SUCCESS_PCT, ANCHOR7_DBM, ANCHOR7_NLOS_PCT, //
		ANCHOR8_HEARD, ANCHOR8_SUCCESS_PCT, ANCHOR8_DBM, ANCHOR8_NLOS_PCT, //
		/** Written by OpenEMS: 1 = run, 0 = pause the simulation. */
		SIM_RUNNING, //
		/** Written by OpenEMS: simulation speed x10. */
		SIM_SPEED_X10, //
		/**
		 * Actuators 1..8 (0..100 %) drive objects in the Sense Sim scene that belong
		 * to zone N (doors, gates, roller shutters...). Sense Sim reads them back.
		 */
		ACTUATOR1, //
		ACTUATOR2, //
		ACTUATOR3, //
		ACTUATOR4, //
		ACTUATOR5, //
		ACTUATOR6, //
		ACTUATOR7, //
		ACTUATOR8, //
		/**
		 * 1 (default) = built-in rule: ActuatorN opens (100) while ZoneN is
		 * occupied and closes (0) when it is empty. 0 = only explicit writes.
		 */
		AUTOMATION, //
		/** Cycles seen by the rule (diagnostics). */
		RULE_CYCLES;

		private final Doc doc;

		private ChannelId() {
			this.doc = Doc.of(OpenemsType.INTEGER).accessMode(AccessMode.READ_WRITE);
		}

		@Override
		public Doc doc() {
			return this.doc;
		}
	}

	public RtlsSenseSimImpl() {
		super(OpenemsComponent.ChannelId.values(), ChannelId.values());
	}

	public void activate(ComponentContext context, Map<String, Object> config) {
		var id = String.valueOf(config.getOrDefault("id", "rtls0"));
		var alias = String.valueOf(config.getOrDefault("alias", ""));
		var enabled = !"false".equals(String.valueOf(config.getOrDefault("enabled", "true")));
		super.activate(context, id, alias, enabled);
		// Every channel publishes what is written to it (Sense Sim writes the
		// metrics, OpenEMS writes SimRunning / SimSpeedX10 / actuators).
		for (var c : ChannelId.values()) {
			IntegerWriteChannel ch = this.channel(c);
			ch.onSetNextWrite(v -> ch.setNextValue(v));
		}
		IntegerWriteChannel automation = this.channel(ChannelId.AUTOMATION);
		automation.setNextValue(1);
	}

	/**
	 * Built-in rule, every cycle before the controllers: ActuatorN opens (100)
	 * while ZoneN is occupied and closes (0) when it is empty, unless
	 * Automation = 0 (then only explicit writes move the actuators).
	 */
	@Override
	public void handleEvent(Event event) {
		IntegerWriteChannel cycles = this.channel(ChannelId.RULE_CYCLES);
		Integer n = cycles.getNextValue().get();
		cycles.setNextValue(n == null ? 1 : n + 1);
		if (!this.isEnabled() || !event.getTopic().endsWith("/BEFORE_CONTROLLERS")) {
			return;
		}
		IntegerWriteChannel automation = this.channel(ChannelId.AUTOMATION);
		Integer auto = automation.value().get();
		if (auto != null && auto == 0) {
			return;
		}
		for (var k = 0; k < ZONES.length; k++) {
			IntegerWriteChannel zone = this.channel(ZONES[k]);
			Integer occupied = zone.value().get();
			if (occupied != null) {
				IntegerWriteChannel act = this.channel(ACTUATORS[k]);
				act.setNextValue(occupied > 0 ? 100 : 0);
			}
		}
	}

	@Override
	public void deactivate() {
		super.deactivate();
	}

	@Override
	public String debugLog() {
		IntegerWriteChannel located = this.channel(ChannelId.TAGS_LOCATED);
		IntegerWriteChannel total = this.channel(ChannelId.TAGS_TOTAL);
		return "tags " + located.value() + "/" + total.value();
	}
}
