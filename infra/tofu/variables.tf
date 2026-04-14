variable "aws_region" {
  description = "AWS region for the MVP deployment."
  type        = string
}

variable "aws_profile" {
  description = "Optional local AWS profile for plan/apply."
  type        = string
  default     = null
  nullable    = true
}

variable "name_prefix" {
  description = "Short prefix used for AWS resource names."
  type        = string
  default     = "ancilla"
}

variable "app_env" {
  description = "Ancilla runtime environment label."
  type        = string
  default     = "production"
}

variable "create_network" {
  description = "When true, create a dedicated MVP VPC, subnets, route tables, and a single NAT gateway."
  type        = bool
  default     = true
}

variable "vpc_id" {
  description = "Existing VPC ID to use when create_network is false."
  type        = string
  default     = null
  nullable    = true
}

variable "public_subnet_ids" {
  description = "Existing public subnet IDs to use when create_network is false."
  type        = list(string)
  default     = []
}

variable "private_app_subnet_ids" {
  description = "Existing private application subnet IDs to use when create_network is false."
  type        = list(string)
  default     = []
}

variable "private_db_subnet_ids" {
  description = "Existing private database subnet IDs to use when create_network is false."
  type        = list(string)
  default     = []
}

variable "vpc_cidr" {
  description = "CIDR block for the MVP VPC when create_network is true."
  type        = string
  default     = "10.42.0.0/16"
}

variable "availability_zones" {
  description = "Availability zones for the created network. Use at least two AZs."
  type        = list(string)
  default     = ["us-west-2a", "us-west-2b"]
}

variable "public_subnet_cidrs" {
  description = "CIDR blocks for public subnets in the created network."
  type        = list(string)
  default     = ["10.42.0.0/24", "10.42.1.0/24"]
}

variable "private_app_subnet_cidrs" {
  description = "CIDR blocks for private ECS/application subnets in the created network."
  type        = list(string)
  default     = ["10.42.10.0/24", "10.42.11.0/24"]
}

variable "private_db_subnet_cidrs" {
  description = "CIDR blocks for private Aurora subnets in the created network."
  type        = list(string)
  default     = ["10.42.20.0/24", "10.42.21.0/24"]
}

variable "allowed_ingress_cidr_blocks" {
  description = "CIDR blocks allowed to reach the public ALB."
  type        = list(string)
  default     = ["0.0.0.0/0"]
}

variable "app_port" {
  description = "Ancilla HTTP port inside the container."
  type        = number
  default     = 3000
}

variable "container_image" {
  description = "Optional full container image URI. Leave null to use the managed ECR repository URL plus tag."
  type        = string
  default     = null
  nullable    = true
}

variable "container_image_tag" {
  description = "Container image tag when using the managed ECR repository."
  type        = string
  default     = "latest"
}

variable "task_cpu" {
  description = "Fargate task CPU units."
  type        = number
  default     = 1024
}

variable "task_memory" {
  description = "Fargate task memory in MiB."
  type        = number
  default     = 2048
}

variable "desired_count" {
  description = "Desired ECS task count."
  type        = number
  default     = 1
}

variable "log_retention_days" {
  description = "CloudWatch Logs retention for the app container."
  type        = number
  default     = 14
}

variable "bedrock_chat_model_id" {
  description = "Bedrock Converse model ID for Ancilla chat responses."
  type        = string
}

variable "bedrock_chat_max_tokens" {
  description = "Default Bedrock max output tokens."
  type        = number
  default     = 800
}

variable "bedrock_chat_temperature" {
  description = "Default Bedrock temperature."
  type        = number
  default     = 0.2
}

variable "db_name" {
  description = "Aurora PostgreSQL database name."
  type        = string
  default     = "ancilla"
}

variable "db_username" {
  description = "Aurora PostgreSQL master username."
  type        = string
  default     = "ancilla"
}

variable "aurora_engine_version" {
  description = "Aurora PostgreSQL engine version. Use a version that supports pgvector."
  type        = string
  default     = "15.8"
}

variable "aurora_instance_class" {
  description = "Aurora instance class for the writer."
  type        = string
  default     = "db.t4g.medium"
}

variable "backup_retention_period" {
  description = "Aurora backup retention period in days."
  type        = number
  default     = 1
}
