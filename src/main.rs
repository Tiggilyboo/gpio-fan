use gpio_cdev::{Chip, Line};
use std::iter::Sum;
use std::{env::args, time::Duration};
use sysinfo::{Component, ComponentExt, Cpu, CpuExt, System, SystemExt};

#[derive(Debug)]
struct Measurement {
    measures: Vec<f32>,
    avg: f32,
    max: usize,
}

impl Measurement {
    pub fn new(max: usize) -> Self {
        Self {
            measures: Vec::with_capacity(max),
            avg: 0f32,
            max,
        }
    }

    pub fn update(&mut self, measurement: f32) -> f32 {
        if self.measures.len() > self.max {
            self.measures.drain(0..0);
        }
        self.measures.push(measurement);

        self.avg = self.measures.iter().copied().sum();
        self.avg /= self.measures.len() as f32;
        self.avg
    }

    pub fn interval(&self) -> usize {
        self.max
    }

    pub fn measurement(&self) -> f32 {
        self.avg
    }
}

struct Usage {
    system: System,
    cpu: Vec<Measurement>,
    temperature: Vec<Measurement>,
    max_temp: Option<f32>,
}

const CPU_COMPONENT_LABEL: &str = "coretemp";

impl Usage {
    pub fn new(cpu_intervals_sec: Vec<usize>, temp_intervals_sec: Vec<usize>) -> Self {
        let mut cpu = Vec::new();
        for interval in cpu_intervals_sec {
            cpu.push(Measurement::new(interval));
        }

        let mut temperature = Vec::new();
        for interval in temp_intervals_sec {
            temperature.push(Measurement::new(interval));
        }

        Self {
            cpu,
            temperature,
            system: System::new_all(),
            max_temp: None,
        }
    }

    pub fn update(&mut self) {
        self.system.refresh_cpu();
        self.system.refresh_system();
        self.system.refresh_components();

        let mut max_cpu_usage: Option<f32> = None;
        for cpu in self.system.cpus() {
            if let Some(cpu_usage) = max_cpu_usage {
                if cpu.cpu_usage() > cpu_usage {
                    max_cpu_usage = Some(cpu.cpu_usage());
                }
            } else {
                max_cpu_usage = Some(cpu.cpu_usage());
            }
        }

        let mut max_cpu_temps: Option<f32> = None;
        let mut min_cpu_max = self.max_temp;
        for c in self.system.components() {
            if c.label().starts_with(CPU_COMPONENT_LABEL) {
                if max_cpu_temps.is_none() || c.temperature() > max_cpu_temps.unwrap() {
                    max_cpu_temps = Some(c.temperature());
                }
                if min_cpu_max.is_none() || c.max() < min_cpu_max.unwrap() {
                    min_cpu_max = Some(c.max());
                }
            }
        }

        if let Some(max_cpu_usage) = max_cpu_usage {
            for cpu in self.cpu.iter_mut() {
                cpu.update(max_cpu_usage);
            }
        }
        if let Some(max_cpu_temps) = max_cpu_temps {
            for temp in self.temperature.iter_mut() {
                temp.update(max_cpu_temps);
            }
        }
    }

    pub fn cpu_max_temp(&self) -> Option<f32> {
        self.max_temp
    }
}

struct FanControl {
    usage: Usage,
    chip: Chip,
    fan_output: Line,
    fan_on: Option<bool>,
    max_fan_on_temp: f32,
    max_fan_on_cpu: f32,
}

impl FanControl {
    pub fn new(
        chip: String,
        line: u32,
        usage: Usage,
        max_fan_on_temp: f32,
        max_fan_on_cpu: f32,
    ) -> Result<Self, gpio_cdev::Error> {
        let mut chip = Chip::new(chip)?;
        let fan_output = chip.get_line(line)?;

        Ok(Self {
            usage,
            chip,
            fan_output,
            fan_on: None,
            max_fan_on_temp,
            max_fan_on_cpu,
        })
    }

    fn update_fan(&mut self, state: bool) -> Option<bool> {
        self.fan_on = Some(state);

        self.fan_on
    }

    pub fn update(&mut self) -> Option<bool> {
        self.usage.update();

        // Find maximum temperature to use
        let mut max_temp = self.max_fan_on_temp;
        if let Some(usage_max) = self.usage.cpu_max_temp() {
            if usage_max < max_temp {
                max_temp = usage_max;
            }
        }

        // Any temperature above maximum?
        if self
            .usage
            .temperature
            .iter()
            .any(|t| t.measurement() > max_temp)
        {
            return self.update_fan(true);
        }

        // CPU Usage > max
        if self
            .usage
            .cpu
            .iter()
            .any(|u| u.measurement() > self.max_fan_on_cpu)
        {
            return self.update_fan(true);
        }

        // Use middle measurement
        if let Some(fan_on) = self.fan_on {
            let first = self.usage.temperature.first().map(|t| t.measurement());
            let middle = self
                .usage
                .temperature
                .get(self.usage.temperature.len() / 2)
                .map(|t| t.measurement());

            // Latest rolling average > max / 2 && > next rolling
            let on = first
                .is_some_and(|f| f > self.max_fan_on_temp / 2f32 && middle.is_some_and(|m| f > m));

            self.update_fan(on)
        } else {
            // Fan's not been used yet, turn it off
            self.update_fan(false)
        }
    }

    pub fn fan_on(&self) -> Option<bool> {
        self.fan_on
    }

    pub fn usage(&self) -> &Usage {
        &self.usage
    }
}

fn verbose(fan_control: &FanControl) {
    let usage = fan_control.usage();
    let cpu_measurements: Vec<f32> = usage.cpu.iter().map(|c| c.measurement()).collect();
    let temp_measurements: Vec<f32> = usage.temperature.iter().map(|t| t.measurement()).collect();
    let fan_verbose = match fan_control.fan_on() {
        Some(true) => "ON",
        Some(false) => "OFF",
        _ => "--",
    };
    println!(
        "[{}] {:?}, {:?}",
        fan_verbose, cpu_measurements, temp_measurements
    );
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let max_history = 100;

    let cpu_intervals = vec![3, 10, 60];
    let temp_intervals = vec![5, 30, 60];
    let cpu_crit = 60f32;
    let mut usage = Usage::new(cpu_intervals, temp_intervals);

    let chip = "/dev/gpiochip0";
    let max_fan_on_temp = 31f32;
    let max_fan_on_cpu = 10f32;
    let mut fan_control =
        FanControl::new(chip.to_string(), 1, usage, max_fan_on_temp, max_fan_on_cpu).unwrap();

    loop {
        fan_control.update();
        verbose(&fan_control);

        std::thread::sleep(Duration::from_secs(1));
    }
}
