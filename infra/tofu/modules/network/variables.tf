variable "name_prefix" {
  description = "Short prefix used for network resource names."
  type        = string
}

variable "aws_region" {
  description = "AWS region for the network resources."
  type        = string
}

variable "app_env" {
  description = "Environment label."
  type        = string
}

variable "vpc_cidr" {
  description = "CIDR block for the VPC."
  type        = string
}

variable "availability_zones" {
  description = "Availability zones used for all subnet tiers."
  type        = list(string)
}

variable "public_subnet_cidrs" {
  description = "CIDR blocks for public subnets."
  type        = list(string)
}

variable "private_app_subnet_cidrs" {
  description = "CIDR blocks for private application subnets reserved for future private service deployments."
  type        = list(string)
}

variable "private_db_subnet_cidrs" {
  description = "CIDR blocks for private PostgreSQL subnets."
  type        = list(string)
}
