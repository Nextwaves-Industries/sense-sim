package com.nextwaves.sensesim.gpu;

import java.util.Map;

import org.osgi.service.component.ComponentContext;
import org.osgi.service.event.Event;
import org.osgi.service.event.EventHandler;

import io.openems.common.channel.AccessMode;
import io.openems.common.types.MeterType;
import io.openems.common.types.OpenemsType;
import io.openems.edge.common.channel.Doc;
import io.openems.edge.common.channel.IntegerWriteChannel;
import io.openems.edge.common.component.AbstractOpenemsComponent;
import io.openems.edge.common.component.OpenemsComponent;
import io.openems.edge.meter.api.ElectricityMeter;

/**
 * The graphics card(s) of the PC running Sense Sim, as an OpenEMS consumption
 * meter.
 *
 * <p>
 * Sense Sim reads the GPU telemetry (nvidia-smi) and writes it through
 * Controller.Api.Rest.ReadWrite (POST /rest/channel/gpu0/PowerW {"value":
 * 72}). Every cycle PowerW is copied to the meter's ActivePower, so the GPU
 * shows up in the energy flow, timedata and controllers like any other
 * consumer. FpsLimit goes the other way: OpenEMS (or a user) writes it and
 * Sense Sim caps its frame rate, i.e. its GPU load (0 = no limit).
 */
public class GpuSenseSimImpl extends AbstractOpenemsComponent implements ElectricityMeter, OpenemsComponent, EventHandler {

	public enum ChannelId implements io.openems.edge.common.channel.ChannelId {
		/** 1 while Sense Sim delivers live telemetry. */
		ONLINE, //
		/** Number of GPUs measured. */
		GPU_COUNT, //
		/** Board power of all measured GPUs (W). */
		POWER_W, //
		/** Power limit of the render GPU (W). */
		POWER_LIMIT_W, //
		/** Render GPU: core and memory utilization (%), temperature, memory, clock, fan. */
		UTILIZATION_PCT, //
		MEMORY_UTIL_PCT, //
		TEMPERATURE_C, //
		MEMORY_USED_MB, //
		MEMORY_TOTAL_MB, //
		CLOCK_MHZ, //
		FAN_PCT, //
		/** Sense Sim viewport frames per second. */
		RENDER_FPS, //
		/** 1 if the render GPU supports hardware ray tracing (RTX). */
		RAY_TRACING, //
		/** 1 if the vision models run on the GPU. */
		VISION_ON_GPU, //
		/** Written by OpenEMS: frame-rate cap for Sense Sim (0 = none). */
		FPS_LIMIT;

		private final Doc doc;

		private ChannelId() {
			this.doc = Doc.of(OpenemsType.INTEGER).accessMode(AccessMode.READ_WRITE);
		}

		@Override
		public Doc doc() {
			return this.doc;
		}
	}

	public GpuSenseSimImpl() {
		super(OpenemsComponent.ChannelId.values(), ElectricityMeter.ChannelId.values(), ChannelId.values());
	}

	public void activate(ComponentContext context, Map<String, Object> config) {
		var id = String.valueOf(config.getOrDefault("id", "gpu0"));
		var alias = String.valueOf(config.getOrDefault("alias", ""));
		var enabled = !"false".equals(String.valueOf(config.getOrDefault("enabled", "true")));
		super.activate(context, id, alias, enabled);
		for (var c : ChannelId.values()) {
			IntegerWriteChannel ch = this.channel(c);
			ch.onSetNextWrite(v -> ch.setNextValue(v));
		}
		IntegerWriteChannel limit = this.channel(ChannelId.FPS_LIMIT);
		limit.setNextValue(0);
	}

	@Override
	public MeterType getMeterType() {
		return MeterType.CONSUMPTION_METERED;
	}

	/** Every cycle before the process image: PowerW becomes the meter's ActivePower. */
	@Override
	public void handleEvent(Event event) {
		if (!this.isEnabled()) {
			return;
		}
		IntegerWriteChannel online = this.channel(ChannelId.ONLINE);
		IntegerWriteChannel power = this.channel(ChannelId.POWER_W);
		Integer on = online.getNextValue().get();
		Integer w = power.getNextValue().get();
		this._setActivePower(on != null && on == 1 ? w : null);
	}

	@Override
	public void deactivate() {
		super.deactivate();
	}

	@Override
	public String debugLog() {
		IntegerWriteChannel power = this.channel(ChannelId.POWER_W);
		IntegerWriteChannel util = this.channel(ChannelId.UTILIZATION_PCT);
		return "gpu " + power.value() + " W, " + util.value() + " %";
	}
}
